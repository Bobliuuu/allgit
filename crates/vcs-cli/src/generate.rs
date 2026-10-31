//! `vcs gen`: shell completions and man pages, generated from the clap definition.
use clap::{Command, Subcommand};
use clap_complete::Shell;
use std::path::PathBuf;
use vcs_repo::Error;

#[derive(Subcommand, Clone)]
pub enum GenCmd {
    /// Print a shell completion script to stdout (bash, zsh, fish, elvish, powershell)
    Completions { shell: Shell },
    /// Print the vcs(1) man page, or with --dir write one page per command
    Man {
        /// Write vcs.1 and vcs-<command>.1 into this directory instead of printing
        #[arg(long)]
        dir: Option<PathBuf>,
    },
}

pub fn run(cmd: GenCmd, mut root: Command) -> Result<(), Error> {
    match cmd {
        GenCmd::Completions { shell } => {
            clap_complete::generate(shell, &mut root, "vcs", &mut std::io::stdout());
        }
        GenCmd::Man { dir: None } => {
            clap_mangen::Man::new(root)
                .render(&mut std::io::stdout())
                .map_err(|e| Error::Msg(e.to_string()))?;
        }
        GenCmd::Man { dir: Some(d) } => {
            std::fs::create_dir_all(&d).map_err(|e| Error::Msg(format!("{}: {e}", d.display())))?;
            root.build();
            write_page(&d, "vcs", &root)?;
            for sub in root.get_subcommands().filter(|s| !s.is_hide_set()) {
                let name = format!("vcs-{}", sub.get_name());
                // clap wants a 'static name; this is a one-shot command, so leaking is harmless.
                let static_name: &'static str = Box::leak(name.clone().into_boxed_str());
                write_page(&d, &name, &sub.clone().name(static_name))?;
            }
        }
    }
    Ok(())
}

fn write_page(dir: &std::path::Path, name: &str, cmd: &Command) -> Result<(), Error> {
    let mut buf = Vec::new();
    clap_mangen::Man::new(cmd.clone())
        .render(&mut buf)
        .map_err(|e| Error::Msg(e.to_string()))?;
    let p = dir.join(format!("{name}.1"));
    std::fs::write(&p, buf).map_err(|e| Error::Msg(format!("{}: {e}", p.display())))
}
