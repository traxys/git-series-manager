use std::{
    fs::OpenOptions,
    io::Write,
    ops::Deref,
    path::{Path, PathBuf},
};

use clap::{Args, Parser, Subcommand};
use config::Config;
use directories::ProjectDirs;
use miette::{miette, Context, IntoDiagnostic, Result};

use temp_dir::TempDir;
use utils::OptExt;

mod utils;

const COVER_LETTER_NAME: &str = "cover-letter";

#[derive(Parser, Debug)]
struct Arg {
    #[arg(
        long,
        global = true,
        help = "git repository to use, defaults to current repository"
    )]
    repo: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Format a patch. alias "p"
    #[command(alias = "p")]
    FormatPatch(FormatPatch),
    /// List all currently known patch series
    #[command(alias = "ls")]
    List(List),
    /// Send a patch series by mail
    Send(Send),
    /// Delete a series
    Delete(Delete),
}

#[derive(Args, Debug)]
struct Delete {
    /// Only delete the local branch of the series
    #[arg(short, long)]
    local_only: bool,
    /// Force the deletion of the branch (-D)
    #[arg(short, long)]
    force: bool,
    /// Branch to delete (defaults to the current branch)
    branch: Option<String>,
}

impl Delete {
    pub fn run(
        self,
        config: GsmConfig,
        git_cd: impl Fn(&[&str]) -> Result<String>,
        patch_dir: &Path,
    ) -> Result<()> {
        let current_branch = git_cd(&["branch", "--show-current"])?;

        let branch = self
            .branch
            .as_ref()
            .try_m_unwrap_or_else(|| Ok(&current_branch))?;

        let has_remote = git_cd(&["rev-parse", "@{u}"]).is_ok();
        if has_remote && !self.local_only {
            println!("Removing branch from remote repository");
            git_cd(&["push", "-d", "origin", branch])?;
        }

        if branch == &current_branch {
            println!("Branch {branch} currently checked out, switching to master");
            git_cd(&[
                "switch",
                config.interdiff_base.as_deref().unwrap_or("master"),
            ])?;
        }

        let branch_delete = match self.force {
            true => "-D",
            false => "-d",
        };

        git_cd(&["branch", branch_delete, branch])?;
        let branch_dir = patch_dir.join(&branch);
        std::fs::remove_dir_all(branch_dir).into_diagnostic()?;

        Ok(())
    }
}

#[derive(Args, Debug)]
struct List {
    #[arg(short, long)]
    verbose: bool,
}

impl List {
    pub fn run(
        self,
        _config: GsmConfig,
        _git_cd: impl Fn(&[&str]) -> Result<String>,
        patch_dir: &Path,
    ) -> Result<()> {
        for entry in patch_dir
            .read_dir()
            .into_diagnostic()
            .wrap_err("Could not read patch dir")?
        {
            let entry = entry
                .into_diagnostic()
                .wrap_err("Could not read patch dir entry")?;

            if entry.file_name() == "config.toml" {
                continue;
            }

            let branch_dir = patch_dir.join(entry.file_name());
            let Some(branch_version) =
                latest_version(&branch_dir).wrap_err("Could not fetch latest version")?
            else {
                continue;
            };

            println!(
                " - {}: v{branch_version}",
                entry.file_name().to_string_lossy()
            );

            if self.verbose {
                println!("   Patches:");
                for entry in branch_dir
                    .join(branch_version.to_string())
                    .read_dir()
                    .into_diagnostic()
                    .wrap_err("Could not read patchset dir")?
                {
                    let entry = entry
                        .into_diagnostic()
                        .wrap_err("Could not read patchset entry")?;

                    println!("    - {}", entry.file_name().to_string_lossy());
                }
            }
        }

        Ok(())
    }
}

#[derive(Args, Debug)]
struct Send {
    #[arg(
        short,
        long,
        help = "Version of the patchset to set. Defaults to the latest version"
    )]
    version: Option<u64>,
    #[arg(help = "Patch series to send. Defaults to the current branch")]
    series: Option<String>,
}

impl Send {
    pub fn run(
        self,
        config: GsmConfig,
        git_cd: impl Fn(&[&str]) -> Result<String>,
        patch_dir: &Path,
    ) -> Result<()> {
        let current_branch = git_cd(&["branch", "--show-current"])?;
        let branch = self
            .series
            .as_ref()
            .try_m_unwrap_or_else(|| Ok(&current_branch))?;

        let branch_dir = patch_dir.join(&branch);
        let version = match self.version {
            Some(v) => v,
            None => match latest_version(&branch_dir)? {
                None => return Err(miette!("No patch set for the branch {branch}")),
                Some(v) => v,
            },
        };

        let version_dir = &branch_dir.join(&version.to_string());

        let mut cmd = std::process::Command::new("git");

        cmd.arg("send-email");
        if let Some(args) = &config.sendmail_args {
            cmd.args(args.iter());
        }
        cmd.arg(version_dir);

        let status = cmd
            .status()
            .into_diagnostic()
            .wrap_err("Could not send emails")?;

        if !status.success() {
            return Err(miette!("Could not send emails"));
        }

        Ok(())
    }
}

#[derive(Args, Debug)]
struct FormatPatch {
    #[arg(short, long, help = "Branch to use (defaults to the current branch)")]
    branch: Option<String>,
    #[arg(short, long, help = "Jenkins job number")]
    ci: Option<u64>,
    #[arg(
        short,
        long,
        help = "Version of the patchset. Defaults to last patchset + 1"
    )]
    version: Option<u64>,
    #[arg(long, help = "Override the current version of the patchset")]
    force: bool,
    #[arg(long, help = "Silence the CI warning")]
    no_ci: bool,
    #[arg(short, long, help = "Send the patches directly")]
    send: bool,
    #[arg(short, long, help = "Perform the interdiff with the supplied version")]
    diff: Option<u64>,
    #[arg(
        short = 'B',
        long,
        help = "Reference for the interdiff (defaults to origin/master)"
    )]
    base_diff: Option<String>,
    extra_args: Vec<String>,
}

impl FormatPatch {
    pub fn run(
        self,
        config: GsmConfig,
        git_cd: impl Fn(&[&str]) -> Result<String>,
        patch_dir: &Path,
    ) -> Result<()> {
        if config.ci_url.is_some() && self.ci.is_none() {
            eprintln!("WARNING: CI was not specified\n");
        }

        let branch = self
            .branch
            .try_m_unwrap_or_else(|| Ok(git_cd(&["branch", "--show-current"])?))?;

        let component = config.component.try_m_unwrap_or_else(|| {
            let url = git_cd(&["remote", "get-url", "origin"])?;
            Ok(url
                .strip_prefix(&config.repo_url_base)
                .ok_or(miette!(
                    "remote {url} does not start with url base {}",
                    config.repo_url_base
                ))?
                .trim_end_matches(".git")
                .to_string())
        })?;

        println!("Component: {component}");
        println!("Branch: {branch}");

        let ci_link = match (config.ci_url, self.ci) {
            (Some(ci_template), Some(id)) => Some(
                ci_template
                    .replace("${component}", &component)
                    .replace("${branch}", &branch)
                    .replace("${ci_job}", &id.to_string()),
            ),
            (Some(ci_template), None) => Some(
                ci_template
                    .replace("${component}", &component)
                    .replace("${branch}", &branch),
            ),
            (None, _) => None,
        };

        let branch_dir = patch_dir.join(&branch);
        std::fs::create_dir_all(&branch_dir)
            .into_diagnostic()
            .wrap_err("could not create branch dir")?;

        let version = match self.version {
            Some(v) => Some(v),
            None => latest_version(&branch_dir)
                .wrap_err("could not get version")?
                .map(|v| v + 1),
        };

        let version_dir = branch_dir.join(version.unwrap_or(1).to_string());

        if version_dir.exists() {
            if !self.force {
                return Err(miette!(
                    "Patch dir {version_dir:?} exists, pass --force to delete it"
                ));
            } else {
                std::fs::remove_dir_all(&version_dir).into_diagnostic()?;
            }
        }

        let version_dir = version_dir
            .to_str()
            .ok_or(miette!("Temp dir is not utf-8"))?;

        struct VersionDir {
            path: PathBuf,
        }

        impl Drop for VersionDir {
            fn drop(&mut self) {
                std::fs::remove_dir_all(&self.path).expect("could not delete version dir on error");
            }
        }

        let format_patch = |extra_args: &[&str]| -> Result<_> {
            let mut format_patch_args = vec!["format-patch", "-o", &version_dir];
            let version_str = version.map(|s| s.to_string());

            if let Some(version) = &version_str {
                format_patch_args.push("-v");
                format_patch_args.push(version);
            }

            let subject_prefix = format!(r#"--subject-prefix=PATCH {component}"#);
            format_patch_args.extend_from_slice(&[&subject_prefix, "--cover-letter"]);
            format_patch_args.extend_from_slice(extra_args);
            format_patch_args.extend(self.extra_args.iter().map(|s| s.deref()));

            git_cd(&format_patch_args)?;

            Ok(VersionDir {
                path: version_dir.into(),
            })
        };

        let _version_dir = if let Some(interdiff) = self.diff {
            let base = self
                .base_diff
                .or(config.interdiff_base)
                .unwrap_or_else(|| String::from("origin/master"));

            struct TempBranch<'a> {
                name: &'a str,
                git: &'a dyn Fn(&[&str]) -> Result<String>,
            }

            impl<'a> Drop for TempBranch<'a> {
                fn drop(&mut self) {
                    (self.git)(&["branch", "-D", self.name]).unwrap();
                }
            }

            let branch = {
                let name = "__patch_old";
                git_cd(&["branch", name, &base])?;
                TempBranch { name, git: &git_cd }
            };

            struct GitWorktree {
                _dir: TempDir,
                path: String,
            }

            impl GitWorktree {
                pub fn exec(&self, args: &[&str]) -> Result<String> {
                    let mut a = vec!["-C", &self.path];
                    a.extend_from_slice(args);

                    git_bare(a)
                }
            }

            impl Drop for GitWorktree {
                fn drop(&mut self) {
                    self.exec(&["worktree", "remove", &self.path]).unwrap();
                }
            }

            let wt = {
                let worktree = temp_dir::TempDir::new()
                    .into_diagnostic()
                    .wrap_err("Could not create worktree directory")?;

                let worktree_path = worktree
                    .path()
                    .to_str()
                    .ok_or(miette!("Temp dir is not utf-8"))?
                    .to_string();

                git_cd(&["worktree", "add", "--detach", &worktree_path])?;

                GitWorktree {
                    path: worktree_path,
                    _dir: worktree,
                }
            };

            wt.exec(&["switch", branch.name])?;

            let patches = branch_dir
                .join(&interdiff.to_string())
                .read_dir()
                .into_diagnostic()
                .wrap_err("Could not read interdiff folder")?
                .map(|e| -> Result<_> {
                    let e = e
                        .into_diagnostic()
                        .wrap_err("Could not read interdiff entry")?;

                    let path = e
                        .path()
                        .to_str()
                        .ok_or(miette!("Interdiff patch path is not utf-8"))?
                        .to_string();

                    match path.contains("cover-letter") {
                        true => Ok(None),
                        false => Ok(Some(path)),
                    }
                })
                .filter_map(|e| e.transpose())
                .collect::<Result<Vec<_>>>()?;
            let mut apply_args = vec!["am", "-3"];
            apply_args.extend(patches.iter().map(|s| s.deref()));
            wt.exec(&apply_args)?;

            let interdiff_branch = format!("--interdiff={}", branch.name);
            format_patch(&[&interdiff_branch])?
        } else {
            format_patch(&[])?
        };

        let cover_letter = branch_dir.join(COVER_LETTER_NAME);
        if !cover_letter.exists() {
            let mut cover_letter_template = format!("Title: \n\nBranch: {branch}\n");
            if let Some(ci_link) = &ci_link {
                cover_letter_template += &format!("CI: {ci_link}\n");
            }

            std::fs::write(&cover_letter, cover_letter_template)
                .into_diagnostic()
                .wrap_err("Could not write cover letter")?;
        }

        std::process::Command::new(config.editor)
            .arg(&cover_letter)
            .status()
            .into_diagnostic()
            .wrap_err("Could not edit cover letter")?;

        let cover_letter = std::fs::read_to_string(cover_letter)
            .into_diagnostic()
            .wrap_err("Error while reading back the cover letter")?;

        let Some((title, body)) = cover_letter.split_once('\n') else {
            return Err(miette!("Missing title newline"));
        };

        let Some(title) = title.strip_prefix("Title: ") else {
            return Err(miette!("Missing `Title: ` prefix"));
        };

        let mut cover_letter = None;
        for entry in Path::new(version_dir)
            .read_dir()
            .into_diagnostic()
            .wrap_err("Could not read patch directory")?
        {
            let entry = entry
                .into_diagnostic()
                .wrap_err("Error while reading patch directory")?;

            if entry
                .file_name()
                .as_encoded_bytes()
                .ends_with(b"cover-letter.patch")
            {
                cover_letter = Some(entry.file_name());
            }
        }

        let cover_letter = Path::new(version_dir)
            .join(cover_letter.ok_or(miette!("Did not find cover letter in {version_dir}"))?);

        let cover_letter_content = std::fs::read_to_string(&cover_letter)
            .into_diagnostic()
            .wrap_err("Could not read patchset cover letter")?;

        let cover_letter_content = cover_letter_content
            .replace("*** SUBJECT HERE ***", title.trim())
            .replace("*** BLURB HERE ***", body.trim());

        let mut file = OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(cover_letter)
            .into_diagnostic()
            .wrap_err("Could not re-open cover letter")?;
        file.write(cover_letter_content.as_bytes())
            .into_diagnostic()
            .wrap_err("Could not save cover letter")?;

        std::mem::forget(_version_dir);

        Ok(())
    }
}

#[derive(Debug, serde::Deserialize)]
struct GsmConfig {
    sendmail_args: Option<Vec<String>>,
    editor: String,
    repo_url_base: String,
    component: Option<String>,
    ci_url: Option<String>,
    interdiff_base: Option<String>,
}

fn latest_version(branch_dir: &Path) -> Result<Option<u64>> {
    let mut dir_content = branch_dir
        .read_dir()
        .into_diagnostic()
        .wrap_err("could not read branch dir")?
        .peekable();

    match dir_content.peek() {
        Some(_) => Ok(Some(
            dir_content
                .filter_map(|e| {
                    let entry = match e.into_diagnostic().wrap_err("Could not read entry") {
                        Ok(e) => e,
                        Err(e) => return Some(Err(e)),
                    };

                    let name = entry.file_name();
                    let name = name.to_str().expect("patch set entry is not utf8");

                    if name == "cover-letter" {
                        None
                    } else {
                        Some(Ok(name.parse().expect("version is not an int")))
                    }
                })
                .try_fold(0, |cur, version| -> Result<_> {
                    let version = version?;

                    Ok(std::cmp::max(version, cur))
                })?,
        )),
        None => Ok(None),
    }
}

fn git_bare(args: Vec<&str>) -> Result<String> {
    let out = duct::cmd("git", args)
        .stderr_to_stdout()
        .unchecked()
        .stdout_capture()
        .run()
        .into_diagnostic()
        .wrap_err("failed to launch git")?;

    let output = String::from_utf8_lossy(&out.stdout);
    let output = output.trim();

    if !out.status.success() {
        return Err(miette!("{output}").wrap_err("git command failed"));
    } else {
        Ok(output.to_string())
    }
}

fn main() -> Result<()> {
    let args = Arg::parse();

    let project_dir = ProjectDirs::from("net", "traxys", "git-series-manager")
        .ok_or(miette!("Could not create project dirs"))?;

    let repo_root = args.repo.try_m_unwrap_or_else(|| {
        Ok(PathBuf::from(git_bare(vec![
            "rev-parse",
            "--show-toplevel",
        ])?))
    })?;
    let patch_dir = repo_root.join(".patches");

    let git_cd = |args: &[&str]| {
        let mut a = vec![
            "-C",
            repo_root
                .to_str()
                .ok_or(miette!("{repo_root:?} is not a valid string"))?,
        ];
        a.extend_from_slice(args);

        git_bare(a)
    };

    let config: GsmConfig = Config::builder()
        .add_source(
            config::File::from(project_dir.config_dir().join("config.toml")).required(false),
        )
        .add_source(config::File::from(patch_dir.join("config.toml")).required(false))
        .build()
        .into_diagnostic()
        .wrap_err("Failed to read the configuration")?
        .try_deserialize()
        .into_diagnostic()
        .wrap_err("Failed to deserialize the configuration")?;

    std::fs::create_dir_all(&patch_dir)
        .into_diagnostic()
        .wrap_err("could not create patch directory")?;

    match args.command {
        Command::FormatPatch(args) => args.run(config, git_cd, &patch_dir),
        Command::List(list) => list.run(config, git_cd, &patch_dir),
        Command::Send(send) => send.run(config, git_cd, &patch_dir),
        Command::Delete(delete) => delete.run(config, git_cd, &patch_dir),
    }
}
