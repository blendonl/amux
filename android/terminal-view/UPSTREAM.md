# terminal-view

This library is Termux's `terminal-view`, vendored from [termux/termux-app](https://github.com/termux/termux-app):

| | |
| --- | --- |
| Tag | `v0.118.3` |
| Commit | `5b657c6adf4304e5198951ce815fe0205dcac29c` |
| Path | `terminal-view/` |
| License | Apache License, Version 2.0, in `android/app/src/main/assets/licenses/termux-terminal.txt` |

`src/` is upstream's `src/` at that commit: the Java sources, the resources and the manifest. `build.gradle.kts` replaces upstream's `build.gradle`, and upstream's `proguard-rules.pro`, which holds only the template's comments, is left out. The rest of termux-app is GPLv3 and none of it is here.

## Modified files

Each modified file starts with a `Modified by amux:` line that says what changed.

- `src/main/java/com/termux/view/TerminalRenderer.java`: kitty placeholder cells (U+10EEEE) draw only their background and the cursor, never the glyph, and are left out of the inverted selection; after the text, `render` draws their images through `ImagePainter`. `render` takes the view's `BitmapCache` and syncs it with the emulator's image store first.
- `src/main/java/com/termux/view/TerminalView.java`: owns the `BitmapCache` and clears it in `attachSession`; `updateSize` also resends the size when the cell pixel size changes with the grid unchanged, so the pty's `ws_xpixel`/`ws_ypixel` stay right; the accessibility text reads placeholder cells as spaces.
- `src/main/java/com/termux/view/textselection/TextSelectionCursorController.java`: copied text turns placeholder cells into spaces.

## New files

These are amux's own files, not upstream's.

- `src/main/java/com/termux/view/PlaceholderLayout.java`: kitty's virtual placement fit: the whole image scaled into its cell box, centred, and clipped to one run of cells.
- `src/main/java/com/termux/view/Pixels.java`: streams raw RGB/RGBA payloads, zlib-compressed or not, into ARGB rows, sampling large images down.
- `src/main/java/com/termux/view/BitmapCache.java`: decoded images by stored image, least recently used first out above 128 MiB, synced with the image store.
- `src/main/java/com/termux/view/BitmapDecoder.java`: decodes stored images into bitmaps, at most 4096 pixels a side.
- `src/main/java/com/termux/view/ImagePainter.java`: draws the images of a frame's placeholder runs, with the selection and the cursor over them.
- `src/main/java/com/termux/view/PlaceholderText.java`: turns placeholder cells in text into spaces.
- `src/test/java/com/termux/view/PlaceholderLayoutTest.java`
- `src/test/java/com/termux/view/PixelsTest.java`
- `src/test/java/com/termux/view/BitmapCacheTest.java`
- `src/test/java/com/termux/view/PlaceholderTextTest.java`
