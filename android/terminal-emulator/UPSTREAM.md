# terminal-emulator

This library is Termux's `terminal-emulator`, vendored from [termux/termux-app](https://github.com/termux/termux-app):

| | |
| --- | --- |
| Tag | `v0.118.3` |
| Commit | `5b657c6adf4304e5198951ce815fe0205dcac29c` |
| Path | `terminal-emulator/` |
| License | Apache License, Version 2.0, in `android/app/src/main/assets/licenses/termux-terminal.txt` |

`src/` is upstream's `src/` at that commit: the Java sources, the unit tests, `src/main/jni` with `termux.c` and `Android.mk`, and the manifest. `build.gradle.kts` replaces upstream's `build.gradle`, and upstream's `proguard-rules.pro`, which holds only the template's comments, is left out. The rest of termux-app is GPLv3 and none of it is here.

## Modified files

Each modified file starts with a `Modified by amux:` line that says what changed.

None yet.
