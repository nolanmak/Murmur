//! Open-at-login via a per-user LaunchAgent that runs `open` on the app bundle.
//! Launching the bundle (not the bare executable) keeps macOS permissions attached
//! to the app. On by default; turning it off is remembered across launches.
use std::{
    io::{self, Write},
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

    /// True only for the registration for this bundle, never just file existence.
    pub fn is_enabled(&self, bundle: &Path) -> bool {
        !self.is_opted_out()
            && std::fs::read_to_string(self.agent_path())
                .is_ok_and(|text| text == agent_plist(bundle))
    }

    pub fn is_opted_out(&self) -> bool {
        self.opt_out_path().exists()
    }

    /// Refresh the agent, or finish removing it when the user opted out.
    /// A failed previous disable must not silently leave an active login file.
    pub fn sync(&self, bundle: &Path) -> io::Result<bool> {
        if self.opt_out_path().try_exists()? {
            return remove_if_present(&self.agent_path());
        }
        self.install(bundle)
    }

    fn install(&self, bundle: &Path) -> io::Result<bool> {
        let plist = agent_plist(bundle);
        let path = self.agent_path();
        if std::fs::read_to_string(&path).is_ok_and(|current| current == plist) {
            return Ok(false);
        }
        let parent = path.parent().expect("agent path has a parent");
        std::fs::create_dir_all(parent)?;
        // Same-directory rename: failed writes cannot truncate a working agent.
        let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
        temporary.write_all(plist.as_bytes())?;
        temporary.as_file().sync_all()?;
        temporary.persist(&path).map_err(|error| error.error)?;
        Ok(true)
    }

    pub fn set_enabled(&self, enabled: bool, bundle: &Path) -> io::Result<()> {
        let opt_out = self.opt_out_path();
        if enabled {
            // Do not forget the explicit opt-out if installation fails.
            self.install(bundle)?;
            if let Err(error) = remove_if_present(&opt_out) {
                // Retain the user's choice if clearing it fails.
                let _ = remove_if_present(&self.agent_path());
                return Err(error);
            }
        } else {
            std::fs::create_dir_all(opt_out.parent().expect("opt-out path has a parent"))?;
            std::fs::write(&opt_out, "")?;
            remove_if_present(&self.agent_path())?;
        }
        Ok(())
    }
}

fn remove_if_present(path: &Path) -> io::Result<bool> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}
