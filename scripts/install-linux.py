"""Install only owned application files; preserve user credentials on uninstall."""
import os
from pathlib import Path
import shutil
import sys

def install(action, binary=None):
    home = Path.home()
    data = Path(os.environ.get("XDG_DATA_HOME", home / ".local/share"))
    if not data.is_absolute():
        data = home / ".local/share"
    target = home / ".local/bin/murmur"
    launcher = data / "applications/murmur.desktop"
    notices = data / "murmur"
    if action == "uninstall":
        target.unlink(missing_ok=True)
        launcher.unlink(missing_ok=True)
        for name in ("LICENSE", "NOTICE"):
            (notices / name).unlink(missing_ok=True)
        if notices.exists() and not any(notices.iterdir()):
            notices.rmdir()
        print("Murmur removed. Your configuration is preserved.")
        return
    if action != "install" or not binary:
        raise SystemExit("Usage: install-linux.py install BINARY | uninstall")
    target.parent.mkdir(parents=True, exist_ok=True)
    launcher.parent.mkdir(parents=True, exist_ok=True)
    notices.mkdir(parents=True, exist_ok=True)
    staged = target.with_suffix(".new")
    shutil.copyfile(binary, staged)
    staged.chmod(0o755)
    staged.replace(target)
    # Desktop Exec is not a shell; quote according to Desktop Entry rules.
    path = str(target).replace("\\", "\\\\\\\\").replace('"', '\\\\"').replace('`', '\\\\`').replace('$', '\\\\$').replace('%', '%%')
    launcher.write_text('[Desktop Entry]\nType=Application\nName=Murmur\nComment=Voice dictation — Linux preview\nExec="' + path + '" run\nIcon=audio-input-microphone\nTerminal=false\nCategories=AudioVideo;Audio;\n')
    root = Path(__file__).resolve().parents[1]
    for name in ("LICENSE", "NOTICE"):
        shutil.copyfile(root / name, notices / name)
    print("Installed Murmur. Open it from the application launcher.")

if __name__ == "__main__":
    install(*sys.argv[1:])
