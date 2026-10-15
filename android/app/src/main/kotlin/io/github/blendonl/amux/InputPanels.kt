package io.github.blendonl.amux

import android.content.res.Configuration
import android.view.View
import android.view.Window
import android.view.WindowManager
import android.view.inputmethod.InputMethodManager
import android.widget.LinearLayout
import com.termux.view.TerminalView
import io.github.blendonl.amux.keyboard.KeyboardHalfView
import io.github.blendonl.amux.keyboard.KeyboardLayout

class InputPanels(
    private val window: Window,
    private val terminalView: TerminalView,
    private val terminalPane: View,
    private val extraKeys: ExtraKeys,
    private val leftHalf: KeyboardHalfView,
    private val rightHalf: KeyboardHalfView,
) {
    private val inputMethods = terminalView.context.getSystemService(InputMethodManager::class.java)
    private var terminalShown = false
    private var splitHidden = false
    private var landscape = false
    private var hardwareKeyboard = false
    private var softInputState: Int? = null

    private val splitMode: Boolean
        get() = landscape && !hardwareKeyboard

    fun configure(configuration: Configuration) {
        val wasSplit = splitMode
        landscape = configuration.orientation == Configuration.ORIENTATION_LANDSCAPE
        hardwareKeyboard = configuration.keyboard != Configuration.KEYBOARD_NOKEYS &&
            configuration.hardKeyboardHidden == Configuration.HARDKEYBOARDHIDDEN_NO
        if (splitMode == wasSplit) return
        splitHidden = false
        apply()
        if (terminalShown && !splitMode) showSystemKeyboard()
    }

    fun resize(layout: KeyboardLayout) {
        setWeight(leftHalf, layout.leftPercent)
        setWeight(terminalPane, layout.terminalPercent)
        setWeight(rightHalf, layout.rightPercent)
    }

    fun showTerminal() {
        terminalShown = true
        splitHidden = false
        apply()
        if (!splitMode) showSystemKeyboard()
    }

    fun hideTerminal() {
        terminalShown = false
        apply()
        hideSystemKeyboard()
    }

    fun onTerminalTap() {
        if (splitMode) {
            splitHidden = false
            apply()
        } else {
            showSystemKeyboard()
        }
    }

    fun hideSplitKeyboard() {
        splitHidden = true
        apply()
    }

    private fun apply() {
        extraKeys.visible = terminalShown && !splitMode
        val split = terminalShown && splitMode && !splitHidden
        for (half in listOf(leftHalf, rightHalf)) {
            if (!split) half.releaseAll()
            half.visibility = if (split) View.VISIBLE else View.GONE
        }
        val state = if (splitMode) {
            WindowManager.LayoutParams.SOFT_INPUT_STATE_ALWAYS_HIDDEN
        } else {
            WindowManager.LayoutParams.SOFT_INPUT_STATE_UNSPECIFIED
        }
        if (state == softInputState) return
        softInputState = state
        val adjust = window.attributes.softInputMode and WindowManager.LayoutParams.SOFT_INPUT_MASK_ADJUST
        window.setSoftInputMode(adjust or state)
        if (splitMode) hideSystemKeyboard()
    }

    private fun showSystemKeyboard() {
        terminalView.post { inputMethods.showSoftInput(terminalView, InputMethodManager.SHOW_IMPLICIT) }
    }

    private fun hideSystemKeyboard() {
        inputMethods.hideSoftInputFromWindow(terminalView.windowToken, 0)
    }

    private fun setWeight(view: View, percent: Float) {
        val params = view.layoutParams as LinearLayout.LayoutParams
        if (params.weight == percent) return
        params.weight = percent
        view.layoutParams = params
    }
}
