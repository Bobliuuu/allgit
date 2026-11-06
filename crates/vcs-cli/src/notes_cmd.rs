//! `vcs notes`: annotations attached to commits (docs/notes.md, docs/object-model.md "Commit notes").

use clap::{Args, Subcommand};
use std::io::Read;
use vcs_repo::Error;

#[derive(Args, Clone)]
pub struct NotesArgs {
    /// Zone whose commits are annotated (default: the zone of the current directory)
    #[arg(long, global = true)]
    pub zone: Option<String>,
    #[command(subcommand)]
    pub cmd: Option<NotesCmd>,
}

#[derive(Args, Clone)]
pub struct MsgArgs {
    /// Note text (repeat for several paragraphs, separated by a blank line)
    #[arg(short, long)]
    pub message: Vec<String>,
    /// Read the note text from a file (`-` for standard input)
    #[arg(short = 'F', long, value_name = "FILE")]
    pub file: Option<String>,
}

#[derive(Subcommand, Clone)]
pub enum NotesCmd {
    /// List notes as `<note blob> <annotated commit>`, or print the note blob id of one commit
    List {
        /// Commit to look up (default: every note)
        rev: Option<String>,
    },
    /// Attach a note to a commit (default HEAD); refuses an existing note unless -f
    Add {
        #[command(flatten)]
        msg: MsgArgs,
        /// Replace an existing note
        #[arg(short, long)]
        force: bool,
        rev: Option<String>,
    },
    /// Print the note on a commit (default HEAD); exit 1 when it has none
    Show { rev: Option<String> },
    /// Add text to the note on a commit, after a blank line (creates the note when there is none)
    Append {
        #[command(flatten)]
        msg: MsgArgs,
        rev: Option<String>,
    },
    /// Remove the note from each commit (default HEAD)
    Remove { revs: Vec<String> },
}

fn text(m: &MsgArgs) -> Result<String, Error> {
    if let Some(f) = &m.file {
        let mut s = String::new();
        if f == "-" {
            std::io::stdin().read_to_string(&mut s).map_err(|source| Error::Io {
                path: "<stdin>".into(),
                source,
            })?;
        } else {
            s = std::fs::read_to_string(f).map_err(|source| Error::Io { path: f.into(), source })?;
        }
        return Ok(s);
    }
    if m.message.is_empty() {
        return Err(Error::Msg("no note text: pass -m <text> or -F <file> (there is no editor)".into()));
    }
    Ok(m.message.join("\n\n"))
}

pub fn run(a: NotesArgs) -> Result<(), Error> {
    let (repo, cwd) = crate::open()?;
    let zone = crate::zone_of_cwd(&repo, &cwd, a.zone)?;
    match a.cmd.unwrap_or(NotesCmd::List { rev: None }) {
        NotesCmd::List { rev: None } => {
            for n in repo.notes_list(&zone)? {
                println!("{} {}", n.blob.to_hex(), n.commit.to_hex());
            }
        }
        NotesCmd::List { rev: Some(r) } => {
            let rev = repo.resolve_rev(&zone, &r)?;
            let found = repo.notes_list(&zone)?.into_iter().find(|n| n.commit == rev);
            match found {
                Some(n) => println!("{}", n.blob.to_hex()),
                None => return Err(Error::Msg(format!("commit {} has no note", rev.short()))),
            }
        }
        NotesCmd::Add { msg, force, rev } => {
            let c = repo.notes_add(&zone, rev.as_deref().unwrap_or("HEAD"), &text(&msg)?, force, crate::author()?)?;
            println!("added note on {}", c.short());
        }
        NotesCmd::Show { rev } => match repo.notes_show(&zone, rev.as_deref().unwrap_or("HEAD"))? {
            Some((_, body)) => {
                use std::io::Write;
                std::io::stdout().write_all(&body).map_err(|source| Error::Io {
                    path: "<stdout>".into(),
                    source,
                })?;
            }
            None => {
                let c = repo.resolve_rev(&zone, rev.as_deref().unwrap_or("HEAD"))?;
                return Err(Error::Msg(format!("commit {} has no note", c.short())));
            }
        },
        NotesCmd::Append { msg, rev } => {
            let c = repo.notes_append(&zone, rev.as_deref().unwrap_or("HEAD"), &text(&msg)?, crate::author()?)?;
            println!("appended to the note on {}", c.short());
        }
        NotesCmd::Remove { revs } => {
            let revs = if revs.is_empty() { vec!["HEAD".to_owned()] } else { revs };
            for r in revs {
                let c = repo.notes_remove(&zone, &r, crate::author()?)?;
                println!("removed note from {}", c.short());
            }
        }
    }
    Ok(())
}
