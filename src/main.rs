#![allow(dead_code)]

mod config;
mod paths;
mod schedule;

fn main() {
    eprintln!("usage: hq [status [--json] | run <job> [--manual] | logs <job> [-f] | hold <job> | resume <job> | sync [--yes] | uninstall]");
    std::process::exit(2);
}
