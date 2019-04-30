#![cfg(feature = "openssl")]

mod initialized {
    use ::sev::{Build, Version, launch, session::Session, certs::*};
    use codicon::Decoder;
    use std::convert::*;

    #[test]
    fn create() {
        Session::try_from(launch::Policy::default()).unwrap();
    }

    #[test]
    fn start() {
        let session = Session::try_from(launch::Policy::default()).unwrap();
        session.start(Chain {
            ca: ca::Chain {
                ark: ca::Certificate::decode(&mut &include_bytes!("naples/ark.cert")[..], ()).unwrap(),
                ask: ca::Certificate::decode(&mut &include_bytes!("naples/ask.cert")[..], ()).unwrap(),
            },
            sev: sev::Chain {
                cek: sev::Certificate::decode(&mut &include_bytes!("naples/cek.cert")[..], ()).unwrap(),
                oca: sev::Certificate::decode(&mut &include_bytes!("naples/oca.cert")[..], ()).unwrap(),
                pek: sev::Certificate::decode(&mut &include_bytes!("naples/pek.cert")[..], ()).unwrap(),
                pdh: sev::Certificate::decode(&mut &include_bytes!("naples/pdh.cert")[..], ()).unwrap(),
            },
        }).unwrap();
    }

    #[test]
    fn verify() {
        let digest = [
            0xe3, 0xb0, 0xc4, 0x42, 0x98, 0xfc, 0x1c, 0x14,
            0x9a, 0xfb, 0xf4, 0xc8, 0x99, 0x6f, 0xb9, 0x24,
            0x27, 0xae, 0x41, 0xe4, 0x64, 0x9b, 0x93, 0x4c,
            0xa4, 0x95, 0x99, 0x1b, 0x78, 0x52, 0xb8, 0x55
        ];

        let measurement = launch::Measurement {
            measure: [
                0x6f, 0xaa, 0xb2, 0xda, 0xae, 0x38, 0x9b, 0xcd,
                0x34, 0x05, 0xa0, 0x5d, 0x6c, 0xaf, 0xe3, 0x3c,
                0x04, 0x14, 0xf7, 0xbe, 0xdd, 0x0b, 0xae, 0x19,
                0xba, 0x5f, 0x38, 0xb7, 0xfd, 0x16, 0x64, 0xea
            ],
            mnonce: [
                0x4f, 0xbe, 0x0b, 0xed, 0xba, 0xd6, 0xc8, 0x6a,
                0xe8, 0xf6, 0x89, 0x71, 0xd1, 0x03, 0xe5, 0x54
            ],
        };

        let policy = launch::Policy {
            flags: launch::PolicyFlags::default(),
            minfw: Version(0, 0),
        };

        let tek = vec![0u8; 16];
        let tik = vec![
            0x66, 0x32, 0x0d, 0xb7, 0x31, 0x58, 0xa3, 0x5a,
            0x25, 0x5d, 0x05, 0x17, 0x58, 0xe9, 0x5e, 0xd4
        ];

        let session = Session::from_keys(policy, tek, tik);
        let build = Build(Version(0x00, 0x12), 0x0f);

        session.verify(&digest, build, measurement).unwrap();
    }
}
