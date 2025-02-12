// SPDX-License-Identifier: Apache-2.0

use primordial::Register;
use sallyport::syscall::{BaseSyscallHandler, ProcessSyscallHandler};
use sallyport::{Cursor, Request};

impl<'a> BaseSyscallHandler for super::Handler<'a> {
    fn translate_shim_to_host_addr<T>(buf: *const T) -> usize {
        buf as _
    }

    fn new_cursor(&mut self) -> Cursor {
        self.block.cursor()
    }

    unsafe fn proxy(&mut self, req: Request) -> sallyport::Result {
        self.block.msg.req = req;

        // prevent earlier writes from being moved beyond this point
        core::sync::atomic::compiler_fence(core::sync::atomic::Ordering::Release);

        asm!("syscall");

        // prevent later reads from being moved before this point
        core::sync::atomic::compiler_fence(core::sync::atomic::Ordering::Acquire);

        self.block.msg.rep.into()
    }

    /// When we are under attack, we trip this circuit breaker and
    /// exit the enclave. Any attempt to re-enter the enclave after
    /// tripping the circuit breaker causes the enclave to immediately
    /// EEXIT.
    fn attacked(&mut self) -> ! {
        self.exit(1)
    }

    #[inline]
    fn unknown_syscall(
        &mut self,
        _a: Register<usize>,
        _b: Register<usize>,
        _c: Register<usize>,
        _d: Register<usize>,
        _e: Register<usize>,
        _f: Register<usize>,
        nr: usize,
    ) {
        debugln!(self, "unsupported syscall: {}", nr);
    }

    fn trace(&mut self, name: &str, argc: usize) {
        let argv = [
            self.gpr.rdi,
            self.gpr.rsi,
            self.gpr.rdx,
            self.gpr.r10,
            self.gpr.r8,
            self.gpr.r9,
        ];

        debug!(self, "{}(", name);
        for (i, arg) in argv[..argc].iter().copied().enumerate() {
            let prefix = if i > 0 { ", " } else { "" };
            debug!(self, "{}0x{:x}", prefix, u64::from(arg));
        }

        debugln!(self, ")");
    }
}
