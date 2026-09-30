package io.github.blendonl.amux

import android.view.View
import android.widget.Button
import android.widget.TextView

class StatusPanel(private val panel: View, onReattach: () -> Unit, onStop: () -> Unit) {
    private val title: TextView = panel.findViewById(R.id.status_title)
    private val detail: TextView = panel.findViewById(R.id.status_detail)
    private val actions: View = panel.findViewById(R.id.status_actions)
    private val reattach: Button = panel.findViewById(R.id.reattach)

    init {
        reattach.setOnClickListener { onReattach() }
        panel.findViewById<View>(R.id.stop).setOnClickListener { onStop() }
    }

    fun show(title: String, detail: String?, reattachLabel: String? = null) {
        this.title.text = title
        this.detail.text = detail
        this.detail.visibility = if (detail == null) View.GONE else View.VISIBLE
        reattach.text = reattachLabel
        actions.visibility = if (reattachLabel == null) View.GONE else View.VISIBLE
        panel.visibility = View.VISIBLE
    }

    fun hide() {
        panel.visibility = View.GONE
    }
}
