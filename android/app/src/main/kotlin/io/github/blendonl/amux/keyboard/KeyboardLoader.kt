package io.github.blendonl.amux.keyboard

class KeyboardLoader(private val print: (defaults: Boolean) -> Result<String>) {
    class Loaded(val layout: KeyboardLayout?, val problem: String?)

    fun load(keepCurrentOnFailure: Boolean): Loaded {
        val configured = layout(defaults = false)
        configured.getOrNull()?.let { return Loaded(it, null) }
        val problem = configured.exceptionOrNull()?.message ?: configured.toString()
        if (keepCurrentOnFailure) return Loaded(null, problem)
        return Loaded(layout(defaults = true).getOrNull(), problem)
    }

    private fun layout(defaults: Boolean): Result<KeyboardLayout> =
        print(defaults).mapCatching(KeyboardLayoutParser::parse)
}
