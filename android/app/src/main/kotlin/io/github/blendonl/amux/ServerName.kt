package io.github.blendonl.amux

import java.text.Normalizer
import java.util.Locale

object ServerName {
    private const val FALLBACK = "android"

    private val combiningMarks = Regex("\\p{Mn}+")
    private val separators = Regex("[^a-z0-9]+")

    fun choose(vararg candidates: String?): String =
        candidates.firstNotNullOfOrNull(::sanitize) ?: FALLBACK

    private fun sanitize(raw: String?): String? =
        Normalizer.normalize(raw.orEmpty(), Normalizer.Form.NFD)
            .replace(combiningMarks, "")
            .lowercase(Locale.ROOT)
            .replace(separators, "-")
            .trim('-')
            .ifEmpty { null }
}
