from pathlib import Path
import subprocess
root=Path(__file__).resolve().parents[1]
files=subprocess.check_output(["git","ls-files"],cwd=root,text=True).splitlines()
for name in files:
    path=Path(name)
    assert not (path.name.startswith(".env") and path.name!=".env.example"), name
    assert path.suffix not in {".pcm",".wav",".p12",".pem"}, name
    assert not any(p in {"target",".local"} or p.endswith(".app") for p in path.parts), name
for name in ["LICENSE","NOTICE","docs/PROVENANCE.md","CONTRIBUTING.md"]:
    assert (root/name).is_file(), name
print("Repository boundaries verified")
