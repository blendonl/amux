package io.github.blendonl.amux

import android.view.KeyEvent
import android.view.View
import com.termux.view.TerminalView

class ExtraKeys(private val row: View, private val terminalView: TerminalView) {
    val control = StickyModifier(row.findViewById(R.id.key_control))
    val alt = StickyModifier(row.findViewById(R.id.key_alt))

    var visible: Boolean
        get() = row.visibility == View.VISIBLE
        set(value) {
            row.visibility = if (value) View.VISIBLE else View.GONE
        }

    init {
        sendsKey(R.id.key_escape, KeyEvent.KEYCODE_ESCAPE)
        sendsKey(R.id.key_tab, KeyEvent.KEYCODE_TAB)
        sendsKey(R.id.key_left, KeyEvent.KEYCODE_DPAD_LEFT)
        sendsKey(R.id.key_down, KeyEvent.KEYCODE_DPAD_DOWN)
        sendsKey(R.id.key_up, KeyEvent.KEYCODE_DPAD_UP)
        sendsKey(R.id.key_right, KeyEvent.KEYCODE_DPAD_RIGHT)
        row.findViewById<View>(R.id.key_prefix).setOnClickListener {
            terminalView.currentSession?.write(PREFIX, 0, PREFIX.size)
        }
    }

    private fun sendsKey(id: Int, keyCode: Int) {
        row.findViewById<View>(id).setOnClickListener {
            terminalView.onKeyDown(keyCode, KeyEvent(KeyEvent.ACTION_DOWN, keyCode))
        }
    }

    private companion object {
        val PREFIX = byteArrayOf(0x02)
    }
}
