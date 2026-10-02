package com.termux.view;

import java.util.Arrays;

final class CodePointWidths {

    static final int MAX_SIZE = 1 << 16;
    private static final int INITIAL_CAPACITY = 256;
    private static final int EMPTY = -1;
    private static final int GOLDEN_RATIO = 0x9E3779B9;

    private int[] mCodePoints;
    private float[] mWidths;
    private int mSize;

    CodePointWidths() {
        allocate(INITIAL_CAPACITY);
    }

    float get(int codePoint) {
        final int mask = mCodePoints.length - 1;
        for (int slot = slotOf(codePoint, mask); ; slot = (slot + 1) & mask) {
            final int stored = mCodePoints[slot];
            if (stored == codePoint) return mWidths[slot];
            if (stored == EMPTY) return Float.NaN;
        }
    }

    void put(int codePoint, float width) {
        if (mSize >= MAX_SIZE) allocate(INITIAL_CAPACITY);
        else if (2 * (mSize + 1) > mCodePoints.length) grow();
        insert(codePoint, width);
    }

    int size() {
        return mSize;
    }

    private void insert(int codePoint, float width) {
        final int mask = mCodePoints.length - 1;
        int slot = slotOf(codePoint, mask);
        while (mCodePoints[slot] != EMPTY && mCodePoints[slot] != codePoint) slot = (slot + 1) & mask;
        if (mCodePoints[slot] == EMPTY) mSize++;
        mCodePoints[slot] = codePoint;
        mWidths[slot] = width;
    }

    private void grow() {
        final int[] codePoints = mCodePoints;
        final float[] widths = mWidths;
        allocate(codePoints.length * 2);
        for (int slot = 0; slot < codePoints.length; slot++) {
            if (codePoints[slot] != EMPTY) insert(codePoints[slot], widths[slot]);
        }
    }

    private void allocate(int capacity) {
        mCodePoints = new int[capacity];
        Arrays.fill(mCodePoints, EMPTY);
        mWidths = new float[capacity];
        mSize = 0;
    }

    private static int slotOf(int codePoint, int mask) {
        final int hash = codePoint * GOLDEN_RATIO;
        return (hash ^ (hash >>> 16)) & mask;
    }

}
