// SPDX-License-Identifier: Apache-2.0

mod enclave;

use crate::backend::sgx::attestation::get_attestation;
use crate::backend::{Command, Datum, Keep};
use crate::binary::*;
use enclave::{Builder, Enclave, Entry, InterruptVector, Registers};

use anyhow::Result;
use goblin::elf::program_header::*;
use lset::{Line, Span};
use primordial::{Page, Pages};
use sallyport::syscall::{SYS_ENARX_CPUID, SYS_ENARX_GETATT};
use sallyport::Block;
use sgx::crypto::Hasher;
use sgx::loader::{self, Loader};
use sgx::types::page::{Class, Flags, SecInfo};
use sgx::types::sig::{Author, Parameters};

use std::arch::x86_64::__cpuid_count;
use std::convert::TryInto;
use std::fmt::Debug;
use std::num::NonZeroU32;
use std::sync::Arc;

mod attestation;
mod data;

struct Segment {
    fline: Line<usize>,
    mline: Line<usize>,
    pages: Pages<Vec<Page>>,
    vpage: usize,
    sinfo: SecInfo,
    flags: flagset::FlagSet<loader::Flags>,
}

impl Debug for Segment {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let letter = |b, c| if b { c } else { ' ' };

        f.write_fmt(format_args!(
            "Segment({:08x}:{:08x} => {:08x}:{:08x} => {:08x}:{:08x} {}{}{}{}{})",
            self.fline.start,
            self.fline.end,
            self.mline.start,
            self.mline.end,
            self.vpage * Page::SIZE,
            self.vpage * Page::SIZE + self.pages.len() * Page::SIZE,
            letter(self.sinfo.flags.contains(Flags::R), 'r'),
            letter(self.sinfo.flags.contains(Flags::W), 'w'),
            letter(self.sinfo.flags.contains(Flags::X), 'x'),
            letter(self.sinfo.class == Class::Tcs, 't'),
            letter(self.flags.contains(loader::Flags::Measure), 'm'),
        ))
    }
}

impl Segment {
    pub fn new(component: &Component, phdr: &ProgramHeader, relocate: usize) -> Self {
        assert_eq!(relocate % Page::SIZE, 0);

        let fline = Line::from(phdr.file_range());
        let mline = Line::from(phdr.vm_range()) >> relocate;
        let skipb = mline.start % Page::SIZE;
        let vpage = mline.start / Page::SIZE;

        let mspan = Span::from(mline);
        let bytes = &component.bytes[phdr.file_range()];
        let pages = Pages::copy_into(bytes, mspan.count, skipb);

        let mut rwx = Flags::empty();
        for (input, output) in [(PF_R, Flags::R), (PF_W, Flags::W), (PF_X, Flags::X)] {
            if phdr.p_flags & input == input {
                rwx |= output;
            }
        }

        Self {
            fline,
            mline,
            pages,
            vpage,
            sinfo: match phdr.p_flags & PF_ENARX_SGX_TCS {
                0 => SecInfo::reg(rwx),
                _ => SecInfo::tcs(),
            },
            flags: match phdr.p_flags & PF_ENARX_SGX_UNMEASURED {
                0 => loader::Flags::Measure.into(),
                _ => None.into(),
            },
        }
    }
}

pub struct Backend;

impl crate::backend::Backend for Backend {
    fn name(&self) -> &'static str {
        "sgx"
    }

    fn shim(&self) -> &'static [u8] {
        include_bytes!(concat!(env!("OUT_DIR"), "/bin/shim-sgx"))
    }

    fn have(&self) -> bool {
        data::dev_sgx_enclave().pass
    }

    fn data(&self) -> Vec<Datum> {
        let mut data = vec![data::dev_sgx_enclave()];

        data.extend(data::CPUIDS.iter().map(|c| c.into()));

        let max = unsafe { __cpuid_count(0x00000000, 0x00000000) }.eax;
        data.push(data::epc_size(max));

        data
    }

    /// Create a keep instance on this backend
    fn build(&self, shim: Component, code: Component) -> Result<Arc<dyn Keep>> {
        // Find the offset for loading the code.
        let slot = Span::from(shim.find_header(PT_ENARX_CODE).unwrap().vm_range());
        assert!(Span::from(code.region()).count <= slot.count);

        // Find the size of the enclave (in powers of two).
        let size: u32 = unsafe { shim.read_note("enarx", NOTE_ENARX_SGX_SIZE)?.unwrap() };
        let size = 1 << size;

        // Find the number of pages in an SSA frame.
        let ssap: u32 = unsafe { shim.read_note("enarx", NOTE_ENARX_SGX_SSAP)?.unwrap() };
        let ssap = NonZeroU32::new(ssap).unwrap();

        // Get an array of all final segment (relative) locations.
        let ssegs = shim
            .filter_header(PT_LOAD)
            .map(|phdr| Segment::new(&shim, phdr, 0));
        let csegs = code
            .filter_header(PT_LOAD)
            .map(|phdr| Segment::new(&code, phdr, slot.start));
        let mut segs: Vec<_> = ssegs.chain(csegs).collect();

        // Ensure no segments overlap in memory.
        segs.sort_unstable_by_key(|x| x.vpage);
        for pair in segs.windows(2) {
            assert!(pair[0].vpage + pair[0].pages.len() <= pair[1].vpage);
        }

        // Initialize the new enclave.
        let parameters = Parameters::default();
        let mut builder = Builder::new(size, ssap, parameters)?;
        let mut hasher = Hasher::new(size, ssap, parameters);

        // Map all the pages.
        for seg in segs {
            builder.load(&seg.pages, seg.vpage, seg.sinfo, seg.flags)?;
            hasher.load(seg.pages, seg.vpage, seg.sinfo, seg.flags)?;
        }

        // Generate a signing key.
        let exp = openssl::bn::BigNum::from_u32(3u32).unwrap();
        let key = openssl::rsa::Rsa::generate_with_e(3072, &exp)?;

        // Create the enclave signature
        let vendor = Author::new(0, 0);
        let signature = hasher.finish().sign(vendor, key)?;

        // Build the enclave.
        Ok(builder.build(&signature)?)
    }
}

impl super::Keep for Enclave {
    fn spawn(self: Arc<Self>) -> Result<Option<Box<dyn crate::backend::Thread>>> {
        let thread = match self.spawn() {
            Some(thread) => thread,
            None => return Ok(None),
        };

        Ok(Some(Box::new(Thread {
            thread,
            registers: Registers::default(),
            block: Block::default(),
            cssa: usize::default(),
            how: Entry::Enter,
        })))
    }
}

struct Thread {
    thread: enclave::Thread,
    registers: Registers,
    block: Block,
    cssa: usize,
    how: Entry,
}

impl Thread {
    fn cpuid(&mut self) {
        unsafe {
            let cpuid = core::arch::x86_64::__cpuid_count(
                self.block.msg.req.arg[0].try_into().unwrap(),
                self.block.msg.req.arg[1].try_into().unwrap(),
            );

            self.block.msg.req.arg[0] = cpuid.eax.into();
            self.block.msg.req.arg[1] = cpuid.ebx.into();
            self.block.msg.req.arg[2] = cpuid.ecx.into();
            self.block.msg.req.arg[3] = cpuid.edx.into();
        }
    }

    fn attest(&mut self) -> Result<()> {
        let result = unsafe {
            get_attestation(
                self.block.msg.req.arg[0].into(),
                self.block.msg.req.arg[1].into(),
                self.block.msg.req.arg[2].into(),
                self.block.msg.req.arg[3].into(),
            )?
        };

        self.block.msg.rep = Ok([result.into(), 0.into()]).into();
        Ok(())
    }
}

impl super::Thread for Thread {
    fn enter(&mut self) -> Result<Command> {
        let prev = self.how;
        self.registers.rdi = (&mut self.block).into();

        self.how = match self.thread.enter(prev, &mut self.registers) {
            Err(ei) if ei.trap == InterruptVector::InvalidOpcode => Entry::Enter,
            Ok(_) => Entry::Resume,
            e => panic!("Unexpected AEX: {:?}", e),
        };

        // Keep track of the CSSA
        match self.how {
            Entry::Enter => self.cssa += 1,
            Entry::Resume => match self.cssa {
                0 => unreachable!(),
                _ => self.cssa -= 1,
            },
        }

        // If we have handled an InvalidOpcode error, evaluate the sallyport.
        if let (Entry::Enter, Entry::Resume) = (prev, self.how) {
            match unsafe { self.block.msg.req }.num.into() {
                SYS_ENARX_CPUID => self.cpuid(),
                SYS_ENARX_GETATT => self.attest()?,
                _ => return Ok(Command::SysCall(&mut self.block)),
            }
        }

        Ok(Command::Continue)
    }
}
