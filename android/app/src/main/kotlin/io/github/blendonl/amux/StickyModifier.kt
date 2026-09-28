package io.github.blendonl.amux

import android.view.View

class StickyModifier(private val key: View) {
    private var active = false
        set(value) {
            field = value
            key.isActivated = value
        }

    init {
        key.setOnClickListener { active = !active }
    }

    fun consume(): Boolean = active.also { active = false }
}
