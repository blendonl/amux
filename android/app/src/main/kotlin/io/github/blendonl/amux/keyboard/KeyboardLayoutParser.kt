package io.github.blendonl.amux.keyboard

import org.json.JSONArray
import org.json.JSONException
import org.json.JSONObject

class KeyboardLayoutException(message: String) : Exception(message)

object KeyboardLayoutParser {
    private const val WIDTH = "width"
    private const val LAYERS = "layers"
    private const val LEFT = "left"
    private const val RIGHT = "right"
    private const val KEY = "key"
    private const val TEXT = "text"
    private const val SEND = "send"
    private const val LABEL = "label"
    private const val SHIFT = "shift"
    private const val REPEATS = "repeats"
    private const val DELETE = 0x7f
    private const val CONTROL_TO_CARET = 0x40

    fun parse(json: String): KeyboardLayout {
        val root = try {
            JSONObject(json)
        } catch (e: JSONException) {
            throw KeyboardLayoutException("not valid JSON: ${e.message}")
        }
        val width = objectField(root, WIDTH, WIDTH)
        val layersJson = objectField(root, LAYERS, LAYERS)
        val layers = buildMap {
            for (name in layersJson.keys()) {
                val path = "$LAYERS.$name"
                val layer = objectField(layersJson, name, path)
                put(name, Layer(name, rows(layer, "$path.$LEFT", LEFT), rows(layer, "$path.$RIGHT", RIGHT)))
            }
        }
        if (KeyboardLayout.BASE !in layers) fail(LAYERS, "has no ${KeyboardLayout.BASE} layer")
        return KeyboardLayout(number(width, LEFT, WIDTH), number(width, RIGHT, WIDTH), layers)
    }

    private fun rows(layer: JSONObject, path: String, side: String): KeyRows {
        val rows = layer.opt(side) as? JSONArray ?: fail(path, "should be a list of rows")
        return List(rows.length()) { row ->
            val rowPath = "$path[${row + 1}]"
            val keys = rows.opt(row) as? JSONArray ?: fail(rowPath, "should be a list of keys")
            List(keys.length()) { column -> key(keys.opt(column), "$rowPath[${column + 1}]") }
        }
    }

    private fun key(value: Any?, path: String): Key = when (value) {
        is String -> named(value, path)
        is JSONObject -> described(value, path)
        else -> fail(path, "should be a key name or a key table")
    }

    private fun named(name: String, path: String): Key =
        if (name.isEmpty()) Key.BLANK else NamedKeys.lookup(name) ?: fail(path, "is \"$name\", which isn't a key name")

    private fun described(json: JSONObject, path: String): Key {
        val key = when {
            json.has(KEY) -> named(json.getString(KEY), "$path.$KEY")
            json.has(TEXT) -> NamedKeys.typing(json.getString(TEXT))
            json.has(SEND) -> json.getString(SEND).let { Key(KeyAction.Send(it), caretNotation(it)) }
            else -> fail(path, "needs one of $KEY, $TEXT and $SEND")
        }
        val shifted = json.optString(SHIFT).takeIf { json.has(SHIFT) }
        val typed = key.action as? KeyAction.Type
        val withShift = if (shifted != null && typed != null) {
            key.copy(action = typed.copy(shifted = shifted), shiftedLabel = shifted)
        } else {
            key
        }
        val label = json.optString(LABEL).takeIf { json.has(LABEL) }
        return withShift.copy(
            label = label ?: withShift.label,
            shiftedLabel = if (label != null && shifted == null) label else withShift.shiftedLabel,
            width = if (json.has(WIDTH)) number(json, WIDTH, path) else withShift.width,
            repeats = if (json.has(REPEATS)) json.getBoolean(REPEATS) else withShift.repeats,
        )
    }

    private fun caretNotation(text: String): String = buildString {
        text.forEach { char ->
            when {
                char.code < ' '.code -> append('^').append((char.code + CONTROL_TO_CARET).toChar())
                char.code == DELETE -> append("^?")
                else -> append(char)
            }
        }
    }

    private fun number(json: JSONObject, field: String, path: String): Float =
        (json.opt(field) as? Number)?.toFloat() ?: fail("$path.$field", "should be a number")

    private fun objectField(json: JSONObject, field: String, path: String): JSONObject =
        json.opt(field) as? JSONObject ?: fail(path, "should be an object")

    private fun fail(path: String, problem: String): Nothing = throw KeyboardLayoutException("$path $problem")
}
