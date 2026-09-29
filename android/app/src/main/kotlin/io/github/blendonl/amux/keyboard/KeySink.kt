package io.github.blendonl.amux.keyboard

interface KeySink {
    fun type(codePoint: Int, ctrl: Boolean, alt: Boolean)

    fun press(keyCode: Int, ctrl: Boolean, alt: Boolean, shift: Boolean)

    fun send(text: String)

    fun paste()

    fun hide()
}
