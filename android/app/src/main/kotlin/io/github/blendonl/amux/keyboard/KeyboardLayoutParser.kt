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
    private const val REPEAT = "repeat"
    private const val MIN_PERCENT = 5f
    private const val MAX_PERCENT = 45f
    private const val DELETE = 0x7f
    private const val CONTROL_TO_CARET = 0x40

    private val ROOT_FIELDS = setOf(WIDTH, LAYERS)
    private val SIDES = setOf(LEFT, RIGHT)
    private val KEY_FIELDS = setOf(KEY, TEXT, SEND, LABEL, SHIFT, WIDTH, REPEAT)
    private val KEY_ACTIONS = listOf(KEY, TEXT, SEND)

    fun parse(defaults: String, overrides: String? = null): KeyboardLayout {
        val root = json(defaults)
        if (overrides != null) merge(root, json(overrides))
        return layout(root)
    }

    private fun json(text: String): JSONObject = try {
        JSONObject(text)
    } catch (e: JSONException) {
        throw KeyboardLayoutException("not valid JSON: ${e.message}")
    }

    private fun merge(into: JSONObject, from: JSONObject) {
        for (name in from.keys()) {
            if (name !in ROOT_FIELDS) fail(name, "is not a setting, expected one of ${ROOT_FIELDS.joinToString()}")
            val source = from.get(name) as? JSONObject ?: fail(name, "should be an object")
            val target = into.optJSONObject(name) ?: JSONObject().also { into.put(name, it) }
            mergeFields(target, source, nested = name == LAYERS)
        }
    }

    private fun mergeFields(into: JSONObject, from: JSONObject, nested: Boolean) {
        for (field in from.keys()) {
            val value = from.get(field)
            val existing = into.optJSONObject(field)
            when {
                from.isNull(field) -> into.remove(field)
                nested && value is JSONObject && existing != null -> mergeFields(existing, value, nested = false)
                else -> into.put(field, value)
            }
        }
    }

    private fun layout(root: JSONObject): KeyboardLayout {
        checkFields(root, null, ROOT_FIELDS)
        val width = objectField(root, WIDTH, WIDTH)
        checkFields(width, WIDTH, SIDES)
        val layersJson = objectField(root, LAYERS, LAYERS)
        val baseJson = layersJson.optJSONObject(KeyboardLayout.BASE)
            ?: fail(LAYERS, "needs a ${KeyboardLayout.BASE} layer")
        val base = layer(KeyboardLayout.BASE, baseJson, null)
        val layers = buildMap {
            put(base.name, base)
            for (name in layersJson.keys()) {
                if (name == base.name) continue
                val layerJson = layersJson.get(name) as? JSONObject ?: fail("$LAYERS.$name", "should be an object")
                put(name, layer(name, layerJson, base))
            }
        }
        checkLayerTargets(layers)
        return KeyboardLayout(percent(width, LEFT), percent(width, RIGHT), layers)
    }

    private fun percent(width: JSONObject, side: String): Float {
        val path = "$WIDTH.$side"
        val value = (width.opt(side) ?: fail(path, "is missing")) as? Number ?: fail(path, "should be a number")
        val percent = value.toFloat()
        if (percent !in MIN_PERCENT..MAX_PERCENT) {
            fail(path, "should be between ${MIN_PERCENT.toInt()} and ${MAX_PERCENT.toInt()} percent of the screen")
        }
        return percent
    }

    private fun layer(name: String, json: JSONObject, base: Layer?): Layer {
        val path = "$LAYERS.$name"
        checkFields(json, path, SIDES)
        return Layer(
            name,
            rows(json, "$path.$LEFT", LEFT, base?.left),
            rows(json, "$path.$RIGHT", RIGHT, base?.right),
        )
    }

    private fun rows(json: JSONObject, path: String, side: String, base: KeyRows?): KeyRows {
        val rows = (json.opt(side) ?: fail(path, "is missing")) as? JSONArray ?: fail(path, "should be a list of rows")
        if (rows.length() == 0) fail(path, "needs at least one row")
        return List(rows.length()) { row ->
            val rowPath = "$path[$row]"
            val baseRow = base?.getOrNull(row)
            when (val value = rows.get(row)) {
                JSONObject.NULL -> baseRow ?: fail(rowPath, transparentProblem(base))
                is JSONArray -> keys(value, rowPath, baseRow, base != null)
                else -> fail(rowPath, "should be a list of keys")
            }
        }
    }

    private fun keys(row: JSONArray, path: String, baseRow: List<Key>?, transparentAllowed: Boolean): List<Key> {
        if (row.length() == 0) fail(path, "needs at least one key")
        return List(row.length()) { column ->
            val keyPath = "$path[$column]"
            when (val value = row.get(column)) {
                JSONObject.NULL ->
                    if (transparentAllowed) baseRow?.getOrNull(column) ?: Key.BLANK else fail(keyPath, transparentProblem(null))
                is String -> named(value, keyPath)
                is JSONObject -> described(value, keyPath)
                else -> fail(keyPath, "should be a key name or an object")
            }
        }
    }

    private fun transparentProblem(base: KeyRows?) =
        if (base == null) "is null, which falls through to the ${KeyboardLayout.BASE} layer, so the ${KeyboardLayout.BASE} layer can't use it"
        else "is null, which falls through to the ${KeyboardLayout.BASE} layer, but that layer has no row there"

    private fun named(name: String, path: String): Key =
        if (name.isEmpty()) Key.BLANK else NamedKeys.lookup(name) ?: fail(path, "is \"$name\", which isn't a key name")

    private fun described(json: JSONObject, path: String): Key {
        checkFields(json, path, KEY_FIELDS)
        val given = KEY_ACTIONS.filter(json::has)
        if (given.size != 1) fail(path, "needs exactly one of ${KEY_ACTIONS.joinToString()}")
        val key = when (val field = given.single()) {
            KEY -> named(string(json, field, path), "$path.$KEY")
            TEXT -> NamedKeys.typing(string(json, field, path).ifEmpty { fail("$path.$TEXT", "is empty") })
            else -> string(json, field, path).let { Key(KeyAction.Send(it), caretNotation(it)) }
        }
        return key
            .withShift(json, path)
            .withLabel(json, path)
            .withWidth(json, path)
            .withRepeat(json, path)
    }

    private fun Key.withShift(json: JSONObject, path: String): Key {
        if (!json.has(SHIFT)) return this
        val typed = action as? KeyAction.Type ?: fail("$path.$SHIFT", "only applies to keys that type text")
        val shifted = string(json, SHIFT, path)
        return copy(action = typed.copy(shifted = shifted), shiftedLabel = shifted)
    }

    private fun Key.withLabel(json: JSONObject, path: String): Key {
        if (!json.has(LABEL)) return this
        val label = string(json, LABEL, path)
        return copy(label = label, shiftedLabel = if (json.has(SHIFT)) shiftedLabel else label)
    }

    private fun Key.withWidth(json: JSONObject, path: String): Key {
        if (!json.has(WIDTH)) return this
        val width = (json.get(WIDTH) as? Number)?.toFloat()
        if (width == null || width <= 0f) fail("$path.$WIDTH", "should be a number above 0")
        return copy(width = width)
    }

    private fun Key.withRepeat(json: JSONObject, path: String): Key {
        if (!json.has(REPEAT)) return this
        return copy(repeats = json.get(REPEAT) as? Boolean ?: fail("$path.$REPEAT", "should be true or false"))
    }

    private fun string(json: JSONObject, field: String, path: String): String =
        json.get(field) as? String ?: fail("$path.$field", "should be a string")

    private fun caretNotation(text: String): String = buildString {
        text.forEach { char ->
            when {
                char.code < ' '.code -> append('^').append((char.code + CONTROL_TO_CARET).toChar())
                char.code == DELETE -> append("^?")
                else -> append(char)
            }
        }
    }

    private fun checkLayerTargets(layers: Map<String, Layer>) {
        for (layer in layers.values) {
            for (side in Side.entries) {
                layer.rows(side).forEachIndexed { row, keys ->
                    keys.forEachIndexed { column, key ->
                        val target = (key.action as? KeyAction.UseLayer)?.name
                        if (target != null && target !in layers) {
                            fail("$LAYERS.${layer.name}.${side.name.lowercase()}[$row][$column]", "uses the unknown layer \"$target\"")
                        }
                    }
                }
            }
        }
    }

    private fun objectField(json: JSONObject, field: String, path: String): JSONObject =
        (json.opt(field) ?: fail(path, "is missing")) as? JSONObject ?: fail(path, "should be an object")

    private fun checkFields(json: JSONObject, path: String?, allowed: Set<String>) {
        for (field in json.keys()) {
            if (field !in allowed) {
                fail(listOfNotNull(path, field).joinToString("."), "is not a setting, expected one of ${allowed.joinToString()}")
            }
        }
    }

    private fun fail(path: String, problem: String): Nothing = throw KeyboardLayoutException("$path $problem")
}
