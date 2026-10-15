package io.github.blendonl.amux

import android.content.ClipData
import android.content.ClipboardManager
import android.content.Context
import android.os.SystemClock
import android.os.TransactionTooLargeException
import android.widget.Toast
import com.termux.terminal.TerminalEmulator
import com.termux.terminal.TerminalSession
import com.termux.terminal.TerminalSessionClient
import com.termux.view.TerminalView

class TerminalSessionCallbacks(
    private val context: Context,
    private val terminalView: TerminalView,
    private val onFinished: (TerminalSession) -> Unit,
) : TerminalSessionClient {
    private val clipboard = context.getSystemService(ClipboardManager::class.java)
    private val bell = Bell(context)

    override fun onTextChanged(changedSession: TerminalSession) = terminalView.onScreenUpdated()

    override fun onTitleChanged(changedSession: TerminalSession) = Unit

    override fun onSessionFinished(finishedSession: TerminalSession) = onFinished(finishedSession)

    override fun onCopyTextToClipboard(session: TerminalSession, text: String) {
        try {
            clipboard.setPrimaryClip(ClipData.newPlainText(context.getString(R.string.app_name), text))
        } catch (error: RuntimeException) {
            if (error.cause !is TransactionTooLargeException) throw error
            Toast.makeText(context, R.string.clipboard_too_large, Toast.LENGTH_SHORT).show()
        }
    }

    override fun onPasteTextFromClipboard(session: TerminalSession) {
        val clip = clipboard.primaryClip?.takeIf { it.itemCount > 0 } ?: return
        val text = clip.getItemAt(0).coerceToText(context).toString()
        if (text.isNotEmpty()) session.emulator?.paste(text)
    }

    override fun onBell(session: TerminalSession) = bell.ring(SystemClock.uptimeMillis())

    override fun onColorsChanged(session: TerminalSession) = terminalView.invalidate()

    override fun onTerminalCursorStateChange(state: Boolean) = Unit

    override fun getTerminalCursorStyle(): Int = TerminalEmulator.DEFAULT_TERMINAL_CURSOR_STYLE

    override fun logError(tag: String?, message: String?) = TerminalLog.error(tag, message)

    override fun logWarn(tag: String?, message: String?) = TerminalLog.warn(tag, message)

    override fun logInfo(tag: String?, message: String?) = TerminalLog.info(tag, message)

    override fun logDebug(tag: String?, message: String?) = TerminalLog.debug(tag, message)

    override fun logVerbose(tag: String?, message: String?) = TerminalLog.verbose(tag, message)

    override fun logStackTraceWithMessage(tag: String?, message: String?, e: Exception?) =
        TerminalLog.error(tag, message, e)

    override fun logStackTrace(tag: String?, e: Exception?) = TerminalLog.error(tag, null, e)
}
