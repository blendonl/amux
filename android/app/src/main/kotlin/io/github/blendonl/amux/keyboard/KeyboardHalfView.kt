package io.github.blendonl.amux.keyboard

import android.content.Context
import android.graphics.Canvas
import android.graphics.Paint
import android.graphics.RectF
import android.util.AttributeSet
import android.util.TypedValue
import android.view.HapticFeedbackConstants
import android.view.MotionEvent
import android.view.View
import io.github.blendonl.amux.R
import kotlin.math.min

class KeyboardHalfView(context: Context, attrs: AttributeSet?) : View(context, attrs) {
    private class PlacedKey(val key: Key, val bounds: RectF)

    private inner class Repeat(private val key: Key) : Runnable {
        override fun run() {
            keyboard?.repeat(key)
            postDelayed(this, REPEAT_INTERVAL_MS)
        }
    }

    private var keyboard: SplitKeyboard? = null
    private var side = Side.LEFT
    private val placed = mutableListOf<PlacedKey>()
    private var placedRows: KeyRows? = null
    private var placedWidth = 0
    private var placedHeight = 0
    private val pressed = mutableMapOf<Int, PlacedKey>()
    private val repeats = mutableMapOf<Int, Repeat>()

    private val density = resources.displayMetrics.density
    private val gap = GAP_DP * density
    private val radius = RADIUS_DP * density
    private val labelPadding = LABEL_PADDING_DP * density
    private val largestText = TypedValue.applyDimension(TypedValue.COMPLEX_UNIT_SP, LARGEST_TEXT_SP, resources.displayMetrics)
    private val face = RectF()

    private val faceColor = context.getColor(R.color.key_face)
    private val pressedColor = context.getColor(R.color.key_pressed)
    private val labelColor = context.getColor(R.color.key_label)
    private val accentColor = context.getColor(R.color.accent)
    private val lockedLabelColor = context.getColor(R.color.terminal_background)

    private val facePaint = Paint(Paint.ANTI_ALIAS_FLAG)
    private val labelPaint = Paint(Paint.ANTI_ALIAS_FLAG).apply { textAlign = Paint.Align.CENTER }

    init {
        setBackgroundColor(context.getColor(R.color.key_background))
    }

    fun attach(keyboard: SplitKeyboard, side: Side) {
        this.keyboard = keyboard
        this.side = side
        keyboard.addListener { invalidate() }
        invalidate()
    }

    fun releaseAll() {
        pressed.keys.toList().forEach(::release)
    }

    override fun onDraw(canvas: Canvas) {
        val keyboard = keyboard ?: return
        val shifted = keyboard.shifted
        for (placedKey in placedKeys(keyboard)) {
            val key = placedKey.key
            if (key.action == KeyAction.Blank) continue
            val latch = keyboard.latchOf(key)
            val locked = latch?.state == Latch.State.LOCKED
            val down = latch?.held == true || pressed.values.any { it === placedKey }
            face.set(placedKey.bounds)
            face.inset(gap, gap)
            facePaint.color = when {
                locked -> accentColor
                down -> pressedColor
                else -> faceColor
            }
            canvas.drawRoundRect(face, radius, radius, facePaint)
            labelPaint.color = when {
                locked -> lockedLabelColor
                latch?.state == Latch.State.ONE_SHOT -> accentColor
                else -> labelColor
            }
            drawLabel(canvas, key.label(shifted))
        }
    }

    override fun onTouchEvent(event: MotionEvent): Boolean {
        val keyboard = keyboard ?: return false
        when (event.actionMasked) {
            MotionEvent.ACTION_DOWN, MotionEvent.ACTION_POINTER_DOWN -> press(keyboard, event)
            MotionEvent.ACTION_POINTER_UP -> release(event.getPointerId(event.actionIndex))
            MotionEvent.ACTION_UP -> {
                releaseAll()
                performClick()
            }
            MotionEvent.ACTION_CANCEL -> releaseAll()
        }
        return true
    }

    override fun performClick(): Boolean = super.performClick()

    override fun onDetachedFromWindow() {
        releaseAll()
        super.onDetachedFromWindow()
    }

    private fun press(keyboard: SplitKeyboard, event: MotionEvent) {
        val index = event.actionIndex
        val placedKey = keyAt(keyboard, event.getX(index), event.getY(index)) ?: return
        val key = placedKey.key
        if (key.action == KeyAction.Blank) return
        val pointer = event.getPointerId(index)
        pressed[pointer] = placedKey
        performHapticFeedback(HapticFeedbackConstants.KEYBOARD_TAP)
        keyboard.press(key)
        if (key.repeats) {
            val repeat = Repeat(key)
            repeats[pointer] = repeat
            postDelayed(repeat, REPEAT_DELAY_MS)
        }
        invalidate()
    }

    private fun release(pointer: Int) {
        repeats.remove(pointer)?.let(::removeCallbacks)
        val placedKey = pressed.remove(pointer) ?: return
        keyboard?.release(placedKey.key)
        invalidate()
    }

    private fun keyAt(keyboard: SplitKeyboard, x: Float, y: Float): PlacedKey? =
        placedKeys(keyboard).firstOrNull { it.bounds.contains(x, y) }

    private fun placedKeys(keyboard: SplitKeyboard): List<PlacedKey> {
        val rows = keyboard.rows(side)
        if (rows === placedRows && width == placedWidth && height == placedHeight) return placed
        placed.clear()
        placedRows = rows
        placedWidth = width
        placedHeight = height
        val rowHeight = (height - paddingTop - paddingBottom).toFloat() / rows.size
        val usableWidth = (width - paddingLeft - paddingRight).toFloat()
        rows.forEachIndexed { row, keys ->
            val top = paddingTop + row * rowHeight
            val unit = usableWidth / keys.sumOf { it.width.toDouble() }.toFloat()
            var left = paddingLeft.toFloat()
            for (key in keys) {
                val right = left + key.width * unit
                placed += PlacedKey(key, RectF(left, top, right, top + rowHeight))
                left = right
            }
        }
        return placed
    }

    private fun drawLabel(canvas: Canvas, label: String) {
        labelPaint.textSize = min(largestText, face.height() * LABEL_HEIGHT_SHARE)
        val available = face.width() - 2 * labelPadding
        val measured = labelPaint.measureText(label)
        if (measured > available && measured > 0f) labelPaint.textSize *= available / measured
        val baseline = face.centerY() - (labelPaint.descent() + labelPaint.ascent()) / 2
        canvas.drawText(label, face.centerX(), baseline, labelPaint)
    }

    private companion object {
        const val GAP_DP = 2f
        const val RADIUS_DP = 6f
        const val LABEL_PADDING_DP = 3f
        const val LARGEST_TEXT_SP = 18f
        const val LABEL_HEIGHT_SHARE = 0.4f
        const val REPEAT_DELAY_MS = 400L
        const val REPEAT_INTERVAL_MS = 50L
    }
}
