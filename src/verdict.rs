//! Output: one glance. Green checks, and for reds the extracted cause — nothing
//! else. No log spelunking.

use crate::runner::{RunReport, Status};

const RED: &str = "\x1b[31m";
const GREEN: &str = "\x1b[32m";
const YELLOW: &str = "\x1b[33m";
const DIM: &str = "\x1b[2m";
const BOLD: &str = "\x1b[1m";
const RESET: &str = "\x1b[0m";

pub fn print(r: &RunReport) {
    println!();
    for c in &r.checks {
        match c.status {
            Status::Passed => {
                let extra = c
                    .detail
                    .as_deref()
                    .map(|d| format!(" {DIM}({d}){RESET}"))
                    .unwrap_or_default();
                println!("  {GREEN}✓{RESET} {}{extra}", c.label);
            }
            Status::Errored => {
                println!(
                    "  {YELLOW}⚠{RESET} {} {DIM}({}){RESET}",
                    c.label,
                    c.detail.as_deref().unwrap_or("couldn't run")
                );
            }
            Status::Skipped => {
                println!("  {DIM}– {} (skipped){RESET}", c.label);
            }
            Status::Failed => {
                println!(
                    "  {RED}✗ {}{RESET} {DIM}({}){RESET}",
                    c.label,
                    c.detail.as_deref().unwrap_or("failed")
                );
                if let Some(cause) = &c.cause {
                    println!("      {}", cause.headline.trim());
                    for line in &cause.correlated {
                        println!("        {DIM}{line}{RESET}");
                    }
                }
            }
        }
    }

    println!();
    match r.verdict.as_str() {
        "pass" => {
            println!("{GREEN}{BOLD}✓ all passed{RESET} {DIM}({} checks){RESET}", r.checks.len());
        }
        "couldn't-run" => {
            println!(
                "{YELLOW}{BOLD}⚠ couldn't run{RESET} {DIM}({} check(s) — no failures observed){RESET}",
                r.errored
            );
        }
        _ => {
            let mut parts = vec![format!("{} failed", r.failed)];
            if r.errored > 0 {
                parts.push(format!("{} couldn't run", r.errored));
            }
            if r.skipped > 0 {
                parts.push(format!("{} skipped", r.skipped));
            }
            println!("{RED}{BOLD}✗ {}{RESET}", parts.join(" · "));
        }
    }
    println!("{DIM}  logs: {}{RESET}", r.run_dir);
    println!();
}
