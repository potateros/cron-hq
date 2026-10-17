#![allow(dead_code)]

mod config;
mod history;
mod launchd;
mod log;
mod paths;
mod runner;
mod schedule;
mod state;

fn main() {
    eprintln!("usage: hq [status [--json] | run <job> [--manual] | logs <job> [-f] | hold <job> | resume <job> | sync [--yes] | uninstall]");
    std::process::exit(2);
}
