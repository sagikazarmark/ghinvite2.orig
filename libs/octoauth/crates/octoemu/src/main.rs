//! `octoemu`: runs the emulator as a process and prints one JSON line with
//! its base URLs once it listens.
//!
//! ```text
//! octoemu [--port N] [--bind IP] [--seed seed.json] [--idle-timeout SECS] [--refuse-assumed]
//! octoemu --fidelity
//! ```
#![forbid(unsafe_code)]

#[cfg(target_arch = "wasm32")]
fn main() {}

#[cfg(not(target_arch = "wasm32"))]
fn main() -> std::process::ExitCode {
    native::main()
}

#[cfg(not(target_arch = "wasm32"))]
mod native {
    use octoemu::Emulator;
    use octoemu::fidelity::Manifest;
    use octoemu::seed::Seed;
    use serde::Serialize;
    use std::io::Write as _;
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};
    use std::path::PathBuf;
    use std::process::ExitCode;
    use std::time::Duration;

    const USAGE: &str = "\
Usage: octoemu [OPTIONS]

A local emulator of the GitHub surface a GitHub App uses. Prints one JSON line
on stdout once listening: {\"api_base\",\"web_base\",\"admin_base\",\"port\",\"pid\"}.

Options:
  --port N             Port to listen on; 0 picks a free one [default: 0]
  --bind IP            Address to listen on [default: 127.0.0.1]
  --seed PATH          JSON seed file (users, orgs, teams, repos, apps, ...);
                       `compat` is the stub-compatibility world (temporary)
  --idle-timeout SECS  Exit after SECS seconds without a request
  --refuse-assumed     Refuse behaviour fidelity.toml labels `assumed`
  --fidelity           Print the fidelity table and exit
  -h, --help           Print this help
  -V, --version        Print the version
";

    #[derive(Debug, Default)]
    struct Args {
        port: u16,
        bind: Option<IpAddr>,
        seed: Option<PathBuf>,
        idle_timeout: Option<u64>,
        refuse_assumed: bool,
        fidelity: bool,
        help: bool,
        version: bool,
    }

    fn parse(args: impl IntoIterator<Item = String>) -> Result<Args, String> {
        let mut parsed = Args::default();
        let mut args = args.into_iter();
        while let Some(arg) = args.next() {
            let (flag, inline) = match arg.split_once('=') {
                Some((flag, value)) if flag.starts_with("--") => (flag.to_owned(), Some(value)),
                _ => (arg.clone(), None),
            };
            let mut value = |name: &str| {
                inline
                    .map(str::to_owned)
                    .or_else(|| args.next())
                    .ok_or_else(|| format!("{name} needs a value"))
            };
            match flag.as_str() {
                "--port" => {
                    parsed.port = value("--port")?
                        .parse()
                        .map_err(|_| "--port must be 0-65535".to_owned())?;
                }
                "--bind" => {
                    parsed.bind = Some(
                        value("--bind")?
                            .parse()
                            .map_err(|_| "--bind must be an IP address".to_owned())?,
                    );
                }
                "--seed" => parsed.seed = Some(value("--seed")?.into()),
                "--idle-timeout" => {
                    let seconds: u64 = value("--idle-timeout")?
                        .parse()
                        .map_err(|_| "--idle-timeout must be a whole number of seconds")?;
                    if seconds == 0 {
                        return Err("--idle-timeout must be at least 1".into());
                    }
                    parsed.idle_timeout = Some(seconds);
                }
                "--refuse-assumed" if inline.is_none() => parsed.refuse_assumed = true,
                "--fidelity" if inline.is_none() => parsed.fidelity = true,
                "-h" | "--help" => parsed.help = true,
                "-V" | "--version" => parsed.version = true,
                _ => return Err(format!("unknown argument {arg:?}")),
            }
        }
        Ok(parsed)
    }

    /// The startup line, in this field order.
    #[derive(Serialize)]
    struct Ready<'a> {
        api_base: &'a str,
        web_base: &'a str,
        admin_base: &'a str,
        port: u16,
        pid: u32,
    }

    pub(super) fn main() -> ExitCode {
        let args = match parse(std::env::args().skip(1)) {
            Ok(args) => args,
            Err(problem) => {
                eprintln!("octoemu: {problem}\n\n{USAGE}");
                return ExitCode::from(2);
            }
        };
        if args.help {
            print!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        if args.version {
            println!("octoemu {}", env!("CARGO_PKG_VERSION"));
            return ExitCode::SUCCESS;
        }
        if args.fidelity {
            return match Manifest::embedded() {
                Ok(manifest) => {
                    print!("{}", manifest.table());
                    ExitCode::SUCCESS
                }
                Err(error) => {
                    eprintln!("octoemu: {error}");
                    ExitCode::FAILURE
                }
            };
        }
        let seed = match &args.seed {
            None => Seed::new(),
            Some(path) if path.as_os_str() == "compat" => octoemu::compat::seed(),
            Some(path) => match std::fs::read_to_string(path)
                .map_err(|e| e.to_string())
                .and_then(|text| Seed::from_json(&text).map_err(|e| e.to_string()))
            {
                Ok(seed) => seed,
                Err(problem) => {
                    eprintln!("octoemu: seed {}: {problem}", path.display());
                    return ExitCode::from(2);
                }
            },
        };
        let runtime = match tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
        {
            Ok(runtime) => runtime,
            Err(error) => {
                eprintln!("octoemu: {error}");
                return ExitCode::FAILURE;
            }
        };
        runtime.block_on(run(args, seed))
    }

    async fn run(args: Args, seed: Seed) -> ExitCode {
        let bind = args.bind.unwrap_or(IpAddr::V4(Ipv4Addr::LOCALHOST));
        let mut builder = Emulator::builder(seed).bind(SocketAddr::new(bind, args.port));
        if let Some(seconds) = args.idle_timeout {
            builder = builder.idle_timeout(Duration::from_secs(seconds));
        }
        if args.refuse_assumed {
            builder = builder.refuse_assumed();
        }
        let emulator = match builder.start().await {
            Ok(emulator) => emulator,
            Err(error @ octoemu::StartError::Seed(_)) => {
                eprintln!("octoemu: {error}");
                return ExitCode::from(2);
            }
            Err(error) => {
                eprintln!("octoemu: {error}");
                return ExitCode::FAILURE;
            }
        };
        let ready = Ready {
            api_base: emulator.api_base(),
            web_base: emulator.web_base(),
            admin_base: emulator.admin_base(),
            port: emulator.addr().port(),
            pid: std::process::id(),
        };
        let line = serde_json::to_string(&ready).expect("plain strings and numbers serialize");
        let mut stdout = std::io::stdout().lock();
        if writeln!(stdout, "{line}")
            .and_then(|()| stdout.flush())
            .is_err()
        {
            return ExitCode::FAILURE;
        }
        drop(stdout);

        tokio::select! {
            () = emulator.stopped() => {}
            () = terminated() => {
                if let Err(error) = emulator.shutdown().await {
                    eprintln!("octoemu: {error}");
                    return ExitCode::FAILURE;
                }
            }
        }
        ExitCode::SUCCESS
    }

    /// Ctrl-C, or SIGTERM on Unix.
    async fn terminated() {
        let ctrl_c = async {
            let _ = tokio::signal::ctrl_c().await;
        };
        #[cfg(unix)]
        let term = async {
            match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
                Ok(mut signal) => {
                    signal.recv().await;
                }
                Err(_) => std::future::pending().await,
            }
        };
        #[cfg(not(unix))]
        let term = std::future::pending::<()>();
        tokio::select! {
            () = ctrl_c => {}
            () = term => {}
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn args(list: &[&str]) -> Result<Args, String> {
            parse(list.iter().map(|arg| (*arg).to_owned()))
        }

        #[test]
        fn parses_flags() {
            let parsed = args(&[
                "--port",
                "0",
                "--bind=::1",
                "--seed",
                "s.json",
                "--idle-timeout=5",
                "--refuse-assumed",
            ])
            .unwrap();
            assert_eq!(parsed.port, 0);
            assert_eq!(parsed.bind, Some("::1".parse().unwrap()));
            assert_eq!(parsed.seed, Some("s.json".into()));
            assert_eq!(parsed.idle_timeout, Some(5));
            assert!(parsed.refuse_assumed);
            assert!(args(&["--fidelity"]).unwrap().fidelity);
        }

        #[test]
        fn rejects_bad_flags() {
            for bad in [
                &["--port"][..],
                &["--port", "70000"],
                &["--bind", "localhost"],
                &["--idle-timeout", "0"],
                &["--idle-timeout", "soon"],
                &["--refuse-assumed=yes"],
                &["--nope"],
            ] {
                assert!(args(bad).is_err(), "{bad:?}");
            }
        }
    }
}
