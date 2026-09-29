package io.github.blendonl.amux.keyboard

import android.content.ClipboardManager
import android.view.KeyEvent
import com.termux.view.TerminalView

class TerminalKeySink(
    private val terminalView: TerminalView,
    private val clipboard: ClipboardManager,
    private val onHide: () -> Unit,
) : KeySink {
    override fun type(codePoint: Int, ctrl: Boolean, alt: Boolean) {
        if (terminalView.currentSession == null) return
        terminalView.inputCodePoint(codePoint, ctrl, alt)
    }

    override fun press(keyCode: Int, ctrl: Boolean, alt: Boolean, shift: Boolean) {
        if (terminalView.currentSession == null) return
        var metaState = 0
        if (ctrl) metaState = metaState or KeyEvent.META_CTRL_ON or KeyEvent.META_CTRL_LEFT_ON
        if (alt) metaState = metaState or KeyEvent.META_ALT_ON or KeyEvent.META_ALT_LEFT_ON
        if (shift) metaState = metaState or KeyEvent.META_SHIFT_ON or KeyEvent.META_SHIFT_LEFT_ON
        terminalView.onKeyDown(keyCode, KeyEvent(0L, 0L, KeyEvent.ACTION_DOWN, keyCode, 0, metaState))
    }

    override fun send(text: String) {
        terminalView.currentSession?.write(text)
    }

    override fun paste() {
        val clip = clipboard.primaryClip?.takeIf { it.itemCount > 0 } ?: return
        val text = clip.getItemAt(0).coerceToText(terminalView.context).toString()
        if (text.isNotEmpty()) terminalView.currentSession?.emulator?.paste(text)
    }

    override fun hide() = onHide()
}
