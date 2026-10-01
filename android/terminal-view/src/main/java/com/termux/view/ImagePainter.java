package com.termux.view;

import android.graphics.Bitmap;
import android.graphics.Canvas;
import android.graphics.Matrix;
import android.graphics.Paint;

import com.termux.terminal.ImageStore;
import com.termux.terminal.PlaceholderRowScanner;
import com.termux.terminal.TerminalBuffer;
import com.termux.terminal.TerminalEmulator;
import com.termux.terminal.TerminalRow;
import com.termux.terminal.TextStyle;

final class ImagePainter implements PlaceholderRowScanner.RunConsumer {

    private static final int SELECTION_ALPHA = 0x60;

    private final float mCellWidth;
    private final float mCellHeight;
    private final float mTextTop;
    private final PlaceholderLayout mLayout = new PlaceholderLayout();
    private final Matrix mMatrix = new Matrix();
    private final Paint mBitmapPaint = new Paint(Paint.FILTER_BITMAP_FLAG);
    private final Paint mFillPaint = new Paint();

    private Canvas mCanvas;
    private BitmapCache<Bitmap> mBitmaps;
    private ImageStore mStore;
    private int mMaxSide;
    private int mScreenRow;
    private int mSelectionStart;
    private int mSelectionEnd;
    private int mSelectionColor;
    private int mCursorColumn;
    private int mCursorColor;
    private int mCursorShape;

    ImagePainter(float cellWidth, float cellHeight, float textTop) {
        mCellWidth = cellWidth;
        mCellHeight = cellHeight;
        mTextTop = textTop;
    }

    void paint(TerminalEmulator emulator, Canvas canvas, BitmapCache<Bitmap> bitmaps, int topRow,
               int selectionY1, int selectionY2, int selectionX1, int selectionX2) {
        final TerminalBuffer screen = emulator.getScreen();
        final int columns = emulator.mColumns;
        final int cursorRow = emulator.shouldCursorBeVisible() ? emulator.getCursorRow() : Integer.MIN_VALUE;
        boolean begun = false;
        for (int row = topRow, endRow = topRow + emulator.mRows; row < endRow; row++) {
            final TerminalRow line = screen.allocateFullLineIfNecessary(screen.externalToInternalRow(row));
            if (!PlaceholderRowScanner.mayContainPlaceholders(line)) continue;
            if (!begun) {
                begin(emulator, canvas, bitmaps);
                begun = true;
            }
            mScreenRow = row - topRow;
            mSelectionStart = -1;
            mSelectionEnd = -1;
            if (row >= selectionY1 && row <= selectionY2) {
                if (row == selectionY1) mSelectionStart = selectionX1;
                mSelectionEnd = row == selectionY2 ? selectionX2 : columns;
            }
            mCursorColumn = row == cursorRow ? emulator.getCursorCol() : -1;
            PlaceholderRowScanner.scan(line, columns, this);
        }
        if (begun) end();
    }

    private void begin(TerminalEmulator emulator, Canvas canvas, BitmapCache<Bitmap> bitmaps) {
        final int[] palette = emulator.mColors.mCurrentColors;
        mCanvas = canvas;
        mBitmaps = bitmaps;
        mStore = emulator.getImages();
        mMaxSide = Math.min(BitmapDecoder.MAX_SIDE, Math.min(canvas.getMaximumBitmapWidth(), canvas.getMaximumBitmapHeight()));
        mSelectionColor = SELECTION_ALPHA << 24 | (palette[TextStyle.COLOR_INDEX_FOREGROUND] & 0x00ffffff);
        mCursorColor = palette[TextStyle.COLOR_INDEX_CURSOR];
        mCursorShape = emulator.getCursorStyle();
        canvas.save();
        canvas.translate(0, mTextTop);
    }

    private void end() {
        mCanvas.restore();
        mCanvas = null;
        mBitmaps = null;
        mStore = null;
    }

    @Override
    public void accept(int screenColumn, int count, int imageId, int imageRow, int imageColumn) {
        final ImageStore.Image image = mStore.get(imageId);
        if (image != null) drawImage(image, screenColumn, count, imageRow, imageColumn);

        final int selectedStart = Math.max(screenColumn, mSelectionStart);
        final int selectedEnd = Math.min(screenColumn + count - 1, mSelectionEnd);
        if (selectedStart <= selectedEnd) {
            mFillPaint.setColor(mSelectionColor);
            mCanvas.drawRect(selectedStart * mCellWidth, mScreenRow * mCellHeight,
                (selectedEnd + 1) * mCellWidth, (mScreenRow + 1) * mCellHeight, mFillPaint);
        }

        if (mCursorColor != 0 && mCursorColumn >= screenColumn && mCursorColumn < screenColumn + count) drawCursor();
    }

    private void drawImage(ImageStore.Image image, int screenColumn, int count, int imageRow, int imageColumn) {
        final ImageStore.VirtualPlacement placement = image.getPlacement(0);
        final int columns = placement == null ? 0 : placement.mColumns;
        final int rows = placement == null ? 0 : placement.mRows;
        if (!mLayout.layout(image.mWidth, image.mHeight, columns, rows, mCellWidth, mCellHeight,
            mScreenRow, screenColumn, count, imageRow, imageColumn)) return;

        mStore.touch(image);
        final Bitmap bitmap = mBitmaps.get(image, mMaxSide);
        if (bitmap == null) return;

        mMatrix.setScale((mLayout.mImageRight - mLayout.mImageLeft) / bitmap.getWidth(),
            (mLayout.mImageBottom - mLayout.mImageTop) / bitmap.getHeight());
        mMatrix.postTranslate(mLayout.mImageLeft, mLayout.mImageTop);
        mCanvas.save();
        mCanvas.clipRect(mLayout.mClipLeft, mLayout.mClipTop, mLayout.mClipRight, mLayout.mClipBottom);
        mCanvas.concat(mMatrix);
        mCanvas.drawBitmap(bitmap, 0, 0, mBitmapPaint);
        mCanvas.restore();
    }

    private void drawCursor() {
        final float left = mCursorColumn * mCellWidth;
        final float bottom = (mScreenRow + 1) * mCellHeight;
        float right = left + mCellWidth;
        float height = mCellHeight;
        if (mCursorShape == TerminalEmulator.TERMINAL_CURSOR_STYLE_UNDERLINE) height /= 4;
        else if (mCursorShape == TerminalEmulator.TERMINAL_CURSOR_STYLE_BAR) right -= (right - left) * 3 / 4;
        mFillPaint.setColor(mCursorColor);
        mCanvas.drawRect(left, bottom - height, right, bottom, mFillPaint);
    }

}
