//! Operator tool for kekring descriptors. Descriptors are secrets: read them from stdin or a
//! private file and pipe the output straight into the secret store; never pass one as an
//! argument or leave it in shell history.
//!
//! ```text
//! cargo run -q -p kekring --example descriptor -- new NAME          > new descriptor (k1)
//! cargo run -q -p kekring --example descriptor -- add     < OLD     > OLD plus generation hwm+1
//! cargo run -q -p kekring --example descriptor -- promote N < OLD   > current = N
//! cargo run -q -p kekring --example descriptor -- remove N  < OLD   > without generation N
//! cargo run -q -p kekring --example descriptor -- show    < OLD     > fingerprint and key ids
//! cargo run -q -p kekring --example descriptor -- check OLD_FILE NEW_FILE
//! ```
//!
//! Every command that writes a descriptor first checks `validate_transition(old, new)`, so it
//! cannot reuse a version, drop the current generation or promote one in the step that adds it.
use kekring::{Keyring, validate_transition};
use std::io::Read;
use std::process::ExitCode;
use zeroize::Zeroizing;

fn fail(message: impl std::fmt::Display) -> ExitCode {
    eprintln!("descriptor: {message}");
    ExitCode::FAILURE
}

fn stdin() -> Result<Keyring, String> {
    let mut text = Zeroizing::new(String::new());
    std::io::stdin()
        .read_to_string(&mut text)
        .map_err(|_| "cannot read stdin".to_owned())?;
    Keyring::parse(&text).map_err(|e| e.to_string())
}

fn file(path: &str) -> Result<Keyring, String> {
    let text =
        Zeroizing::new(std::fs::read_to_string(path).map_err(|_| format!("cannot read {path}"))?);
    Keyring::parse(&text).map_err(|e| format!("{path}: {e}"))
}

fn root() -> Zeroizing<[u8; 32]> {
    let mut root = Zeroizing::new([0u8; 32]);
    getrandom::fill(&mut root[..]).expect("operating system RNG");
    root
}

/// The descriptor fields after `kekring1`: `name=..`, `current=..`, `hwm=..`, `kN=..`...
fn fields(ring: &Keyring) -> Vec<Zeroizing<String>> {
    ring.to_descriptor()
        .split(';')
        .skip(1)
        .map(|f| Zeroizing::new(f.to_owned()))
        .collect()
}

fn assemble(fields: &[Zeroizing<String>]) -> Result<Keyring, String> {
    let mut text = Zeroizing::new(String::from(kekring::labels::DESCRIPTOR_PREFIX));
    for f in fields {
        text.push(';');
        text.push_str(f);
    }
    Keyring::parse(&text).map_err(|e| e.to_string())
}

fn header(fields: &mut [Zeroizing<String>], key: &str, value: u32) {
    let i = fields
        .iter()
        .position(|f| f.starts_with(&format!("{key}=")))
        .expect("header field");
    fields[i] = Zeroizing::new(format!("{key}={value}"));
}

fn add(old: &Keyring) -> Result<Keyring, String> {
    let v = old
        .high_water_mark()
        .checked_add(1)
        .ok_or("the version space is exhausted")?;
    // A one-generation keyring of the same name yields the `kN=ROOT.KID` field.
    let single = Keyring::builder(old.name())
        .generation(v, root())
        .current(v)
        .build()
        .map_err(|e| e.to_string())?;
    let mut f = fields(old);
    f.push(fields(&single).pop().expect("generation field"));
    header(&mut f, "hwm", v);
    assemble(&f)
}

fn promote(old: &Keyring, v: u32) -> Result<Keyring, String> {
    let mut f = fields(old);
    header(&mut f, "current", v);
    assemble(&f)
}

fn remove(old: &Keyring, v: u32) -> Result<Keyring, String> {
    let mut f = fields(old);
    let before = f.len();
    f.retain(|field| !field.starts_with(&format!("k{v}=")));
    if f.len() == before {
        return Err(format!("generation {v} is not listed"));
    }
    assemble(&f)
}

fn print(ring: &Keyring) {
    println!("{}", *ring.to_descriptor());
}

fn run(args: &[String]) -> Result<(), String> {
    let version = |s: Option<&String>| -> Result<u32, String> {
        s.and_then(|s| s.parse().ok())
            .ok_or_else(|| "expected a version".to_owned())
    };
    let step = |old: Keyring, new: Keyring| -> Result<(), String> {
        validate_transition(&old, &new).map_err(|e| e.to_string())?;
        print(&new);
        Ok(())
    };
    match args.first().map(String::as_str) {
        Some("new") => {
            let name = args.get(1).ok_or("expected a keyring name")?;
            let ring = Keyring::builder(name)
                .generation(1, root())
                .current(1)
                .build()
                .map_err(|e| e.to_string())?;
            print(&ring);
            Ok(())
        }
        Some("add") => {
            let old = stdin()?;
            let new = add(&old)?;
            step(old, new)
        }
        Some("promote") => {
            let old = stdin()?;
            let new = promote(&old, version(args.get(1))?)?;
            step(old, new)
        }
        Some("remove") => {
            let old = stdin()?;
            let new = remove(&old, version(args.get(1))?)?;
            step(old, new)
        }
        Some("show") => {
            let ring = stdin()?;
            println!("{}", ring.fingerprint());
            for v in ring.versions() {
                let kid: String = ring
                    .kid(v)
                    .expect("listed")
                    .iter()
                    .map(|b| format!("{b:02x}"))
                    .collect();
                println!("k{v} kid={kid}");
            }
            Ok(())
        }
        Some("check") => {
            let (old, new) = (
                args.get(1).ok_or("expected OLD_FILE")?,
                args.get(2).ok_or("expected NEW_FILE")?,
            );
            validate_transition(&file(old)?, &file(new)?).map_err(|e| e.to_string())?;
            println!("ok");
            Ok(())
        }
        _ => Err(
            "usage: new NAME | add | promote N | remove N | show | check OLD_FILE NEW_FILE"
                .to_owned(),
        ),
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => fail(message),
    }
}
