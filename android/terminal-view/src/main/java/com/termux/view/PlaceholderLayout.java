package com.termux.view;

final class PlaceholderLayout {

    private static final double CELL_ROUNDING_SLACK = 1e-3;

    int mColumns;
    int mRows;
    float mScale;
    float mImageLeft;
    float mImageTop;
    float mImageRight;
    float mImageBottom;
    float mClipLeft;
    float mClipTop;
    float mClipRight;
    float mClipBottom;

    static int cellsFor(int pixels, float cellPixels) {
        return Math.max(1, (int) Math.ceil(pixels / (double) cellPixels - CELL_ROUNDING_SLACK));
    }

    boolean layout(int imageWidth, int imageHeight, int placementColumns, int placementRows, float cellWidth, float cellHeight,
                   int screenRow, int screenColumn, int count, int imageRow, int imageColumn) {
        final int width = Math.max(1, imageWidth);
        final int height = Math.max(1, imageHeight);
        mColumns = placementColumns > 0 ? placementColumns : cellsFor(width, cellWidth);
        mRows = placementRows > 0 ? placementRows : cellsFor(height, cellHeight);

        final float boxWidth = mColumns * cellWidth;
        final float boxHeight = mRows * cellHeight;
        float offsetX = 0;
        float offsetY = 0;
        if ((double) width * boxHeight > (double) height * boxWidth) {
            mScale = boxWidth / width;
            offsetY = (boxHeight - height * mScale) / 2;
        } else {
            mScale = boxHeight / height;
            offsetX = (boxWidth - width * mScale) / 2;
        }

        mImageLeft = (screenColumn - imageColumn) * cellWidth + offsetX;
        mImageTop = (screenRow - imageRow) * cellHeight + offsetY;
        mImageRight = mImageLeft + width * mScale;
        mImageBottom = mImageTop + height * mScale;

        mClipLeft = screenColumn * cellWidth;
        mClipRight = (screenColumn + count) * cellWidth;
        mClipTop = screenRow * cellHeight;
        mClipBottom = mClipTop + cellHeight;

        return mImageLeft < mClipRight && mClipLeft < mImageRight && mImageTop < mClipBottom && mClipTop < mImageBottom;
    }

}
