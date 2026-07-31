#[cfg(unix)]
#[path = "lunarwing-darkirc-key-helper/unix.rs"]
mod unix;

#[cfg(unix)]
fn main() -> std::process::ExitCode {
    unix::main()
}

#[cfg(not(unix))]
fn main() -> std::process::ExitCode {
    eprintln!("lunarwing-darkirc-key-helper is unsupported on this platform");
    std::process::ExitCode::FAILURE
}
