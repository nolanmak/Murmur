# Browser focus before recording

Local dictation prewarms the focused app’s accessibility tree while idle. At
startup it probes every 50 ms while the field is missing or not editable, using
a 750 ms monotonic budget. An editable field requires no sleep; a known secure
field stops probing immediately. Missing or noneditable focus after the budget
refuses capture with “No text field found · click into a text field and try again”.

The chosen field remains the original insertion target. Completion cannot choose
a field that only appeared after recording. Existing target/window/generation
checks still own delivery. Remote review and automatic RustDesk targeting retain
their separate routes.

Pure regressions first demonstrated recording with unresolved focus and accepting
an editable result after slow probes had exhausted the budget. Injected-clock
checks cover delayed/immediate/secure results and include probe time, rather than
only adding requested sleep durations. They open no apps or microphone.

The deadline cannot interrupt a native accessibility IPC call already in progress;
a result arriving beyond the budget is refused. Current signed-app Chrome/Discord
first-attempt insertion and native IPC timing remain acceptance gates in #49/#58.
This behavior does not establish those gates, and no real browser/Notes profile or
user field was inspected for these tests.
