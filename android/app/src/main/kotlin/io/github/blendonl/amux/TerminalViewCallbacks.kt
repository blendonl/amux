package io.github.blendonl.amux

import android.view.KeyEvent
import android.view.MotionEvent
import com.termux.terminal.TerminalSession
import com.termux.view.TerminalView
import com.termux.view.TerminalViewClient

class TerminalViewCallbacks(
    private val terminalView: TerminalView,
    private val extraKeys: ExtraKeys,
    private val fontSize: FontSize,
    private val onTap: () -> Unit,
) : TerminalViewClient {
    override fun onScale(scale: Float): Float {
        if (scale in PINCH_DEAD_ZONE) return scale
        terminalView.setTextSize(fontSize.adjust(larger = scale > 1f))
        return 1f
    }

    override fun onSingleTapUp(e: MotionEvent) = onTap()

    override fun shouldBackButtonBeMappedToEscape() = false

    override fun shouldEnforceCharBasedInput() = true

    override fun shouldUseCtrlSpaceWorkaround() = false

    override fun isTerminalViewSelected() = true

    override fun copyModeChanged(copyMode: Boolean) = Unit

    override fun onKeyDown(keyCode: Int, e: KeyEvent, session: TerminalSession) = false

    override fun onKeyUp(keyCode: Int, e: KeyEvent) = false

    override fun onLongPress(event: MotionEvent) = false

    override fun readControlKey() = extraKeys.control.consume()

    override fun readAltKey() = extraKeys.alt.consume()

    override fun readShiftKey() = false

    override fun readFnKey() = false

    override fun onCodePoint(codePoint: Int, ctrlDown: Boolean, session: TerminalSession) = false

    override fun onEmulatorSet() = Unit

    override fun logError(tag: String?, message: String?) = TerminalLog.error(tag, message)

    override fun logWarn(tag: String?, message: String?) = TerminalLog.warn(tag, message)

    override fun logInfo(tag: String?, message: String?) = TerminalLog.info(tag, message)

    override fun logDebug(tag: String?, message: String?) = TerminalLog.debug(tag, message)

    override fun logVerbose(tag: String?, message: String?) = TerminalLog.verbose(tag, message)

    override fun logStackTraceWithMessage(tag: String?, message: String?, e: Exception?) =
        TerminalLog.error(tag, message, e)

    override fun logStackTrace(tag: String?, e: Exception?) = TerminalLog.error(tag, null, e)

    private companion object {
        val PINCH_DEAD_ZONE = 0.9f..1.1f
    }
}
