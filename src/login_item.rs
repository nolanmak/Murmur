//! Open-at-login via a per-user LaunchAgent that runs `open` on the app bundle.
//! Launching the bundle (not the bare executable) keeps macOS permissions attached
//! to the app. On by default; turning it off is remembered across launches.
use std::{
    io,
    path::{Path, PathBuf},
};

pub const LABEL: &str = "com.shipsystems.texttospeech.login";

/// The `.app` bundle containing `executable`, if it is `X.app/Contents/MacOS/<exe>`.
pub fn app_bundle(executable: &Path) -> Option<PathBuf> {
    let macos = executable.parent()?;
    let contents = macos.parent()?;
    let bundle = contents.parent()?;
    (macos.file_name()? == "MacOS"
        && contents.file_name()? == "Contents"
        && bundle.extension()? == "app")
        .then(|| bundle.to_path_buf())
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

pub fn agent_plist(bundle: &Path) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>Label</key><string>{LABEL}</string>
<key>ProgramArguments</key><array><string>/usr/bin/open</string><string>{}</string></array>
<key>RunAtLoad</key><true/>
<key>ProcessType</key><string>Interactive</string>
</dict></plist>
"#,
        escape(&bundle.to_string_lossy())
    )
}

pub struct LoginItem {
    home: PathBuf,
}

impl LoginItem {
    pub fn new(home: PathBuf) -> Self {
        Self { home }
    }

    pub fn for_user() -> Option<Self> {
        std::env::var_os("HOME")
            .filter(|home| !home.is_empty())
            .map(|home| Self::new(home.into()))
    }

    pub fn agent_path(&self) -> PathBuf {
        self.home
            .join("Library/LaunchAgents")
            .join(format!("{LABEL}.plist"))
    }

    fn opt_out_path(&self) -> PathBuf {
        self.home
            .join("Library/Application Support/Murmur/open-at-login-disabled")
    }

    pub fn is_enabled(&self) -> bool {
        self.agent_path().is_file()
    }

    /// Install or refresh the agent unless the user turned it off.
    /// Returns whether the agent file changed.
    pub fn sync(&self, bundle: &Path) -> io::Result<bool> {
        if self.opt_out_path().exists() {
            return Ok(false);
        }
        let plist = agent_plist(bundle);
        let path = self.agent_path();
        if std::fs::read_to_string(&path).is_ok_and(|current| current == plist) {
            return Ok(false);
        }
        std::fs::create_dir_all(path.parent().expect("agent path has a parent"))?;
        std::fs::write(&path, plist)?;
        Ok(true)
    }

    pub fn set_enabled(&self, enabled: bool, bundle: &Path) -> io::Result<()> {
        let opt_out = self.opt_out_path();
        if enabled {
            match std::fs::remove_file(&opt_out) {
                Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
                _ => {}
            }
            self.sync(bundle).map(|_| ())
        } else {
            std::fs::create_dir_all(opt_out.parent().expect("opt-out path has a parent"))?;
            std::fs::write(&opt_out, "")?;
            match std::fs::remove_file(self.agent_path()) {
                Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
                _ => Ok(()),
            }
        }
    }
}
