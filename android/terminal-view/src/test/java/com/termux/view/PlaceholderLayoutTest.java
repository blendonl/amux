package com.termux.view;

import junit.framework.TestCase;

public class PlaceholderLayoutTest extends TestCase {

    private static final float DELTA = 1e-4f;

    private final PlaceholderLayout mLayout = new PlaceholderLayout();

    private void assertImage(float left, float top, float right, float bottom) {
        assertEquals(left, mLayout.mImageLeft, DELTA);
        assertEquals(top, mLayout.mImageTop, DELTA);
        assertEquals(right, mLayout.mImageRight, DELTA);
        assertEquals(bottom, mLayout.mImageBottom, DELTA);
    }

    private void assertClip(float left, float top, float right, float bottom) {
        assertEquals(left, mLayout.mClipLeft, DELTA);
        assertEquals(top, mLayout.mClipTop, DELTA);
        assertEquals(right, mLayout.mClipRight, DELTA);
        assertEquals(bottom, mLayout.mClipBottom, DELTA);
    }

    public void testWideImageFitsWidthAndCentresVertically() {
        assertFalse(mLayout.layout(200, 100, 4, 4, 10, 20, 0, 0, 4, 0, 0));
        assertEquals(0.2f, mLayout.mScale, DELTA);
        assertImage(0, 30, 40, 50);
        assertClip(0, 0, 40, 20);

        assertTrue(mLayout.layout(200, 100, 4, 4, 10, 20, 1, 0, 4, 1, 0));
        assertImage(0, 30, 40, 50);
        assertClip(0, 20, 40, 40);

        assertTrue(mLayout.layout(200, 100, 4, 4, 10, 20, 2, 0, 4, 2, 0));
        assertFalse(mLayout.layout(200, 100, 4, 4, 10, 20, 3, 0, 4, 3, 0));
    }

    public void testTallImageFitsHeightAndCentresHorizontally() {
        assertTrue(mLayout.layout(100, 200, 4, 2, 10, 20, 0, 0, 4, 0, 0));
        assertEquals(0.2f, mLayout.mScale, DELTA);
        assertImage(10, 0, 30, 40);
        assertClip(0, 0, 40, 20);

        assertFalse(mLayout.layout(100, 200, 4, 2, 10, 20, 0, 0, 1, 0, 0));
        assertTrue(mLayout.layout(100, 200, 4, 2, 10, 20, 0, 1, 1, 0, 1));
        assertFalse(mLayout.layout(100, 200, 4, 2, 10, 20, 0, 3, 1, 0, 3));
    }

    public void testSmallImageScalesUpToTheBox() {
        assertTrue(mLayout.layout(10, 10, 4, 2, 10, 20, 0, 0, 4, 0, 0));
        assertEquals(4f, mLayout.mScale, DELTA);
        assertImage(0, 0, 40, 40);
    }

    public void testAutoColumnsAndRowsRoundUp() {
        assertTrue(mLayout.layout(25, 41, 0, 0, 10, 20, 0, 0, 3, 0, 0));
        assertEquals(3, mLayout.mColumns);
        assertEquals(3, mLayout.mRows);
        assertEquals(1.2f, mLayout.mScale, DELTA);
        assertImage(0, 5.4f, 30, 54.6f);
    }

    public void testAutoColumnsAndRowsOfAnExactMultiple() {
        assertTrue(mLayout.layout(30, 40, 0, 0, 10, 20, 0, 0, 3, 0, 0));
        assertEquals(3, mLayout.mColumns);
        assertEquals(2, mLayout.mRows);
        assertEquals(1f, mLayout.mScale, DELTA);
        assertImage(0, 0, 30, 40);
    }

    public void testOneAutoAxis() {
        mLayout.layout(30, 40, 6, 0, 10, 20, 0, 0, 6, 0, 0);
        assertEquals(6, mLayout.mColumns);
        assertEquals(2, mLayout.mRows);
        assertImage(15, 0, 45, 40);

        mLayout.layout(30, 40, 0, 4, 10, 20, 0, 0, 3, 0, 0);
        assertEquals(3, mLayout.mColumns);
        assertEquals(4, mLayout.mRows);
        assertImage(0, 20, 30, 60);
    }

    public void testRunOnAMiddleRow() {
        assertTrue(mLayout.layout(40, 80, 4, 4, 10, 20, 7, 2, 4, 2, 0));
        assertImage(20, 100, 60, 180);
        assertClip(20, 140, 60, 160);
    }

    public void testRunOfMiddleColumns() {
        assertTrue(mLayout.layout(40, 80, 4, 4, 10, 20, 3, 5, 2, 1, 1));
        assertImage(40, 40, 80, 120);
        assertClip(50, 60, 70, 80);
    }

    public void testRunStartingPastTheFirstImageColumn() {
        assertTrue(mLayout.layout(40, 20, 4, 1, 10, 20, 0, 0, 2, 0, 2));
        assertImage(-20, 0, 20, 20);
        assertClip(0, 0, 20, 20);
    }

    public void testRunOutsideTheBoxDrawsNothing() {
        assertFalse(mLayout.layout(40, 20, 4, 1, 10, 20, 0, 0, 2, 0, 4));
        assertFalse(mLayout.layout(40, 20, 4, 1, 10, 20, 0, 0, 2, 1, 0));
    }

    public void testFractionalCellWidth() {
        assertTrue(mLayout.layout(30, 40, 0, 0, 7.5f, 20, 1, 3, 2, 1, 1));
        assertEquals(4, mLayout.mColumns);
        assertEquals(2, mLayout.mRows);
        assertEquals(1f, mLayout.mScale, DELTA);
        assertImage(15, 0, 45, 40);
        assertClip(22.5f, 20, 37.5f, 40);

        mLayout.layout(103, 20, 0, 0, 10.299999f, 20, 0, 0, 10, 0, 0);
        assertEquals(10, mLayout.mColumns);
        mLayout.layout(104, 20, 0, 0, 10.299999f, 20, 0, 0, 10, 0, 0);
        assertEquals(11, mLayout.mColumns);
    }

    public void testExactFitHasNoSlack() {
        assertTrue(mLayout.layout(75, 60, 10, 3, 7.5f, 20, 4, 0, 10, 1, 0));
        assertEquals(1f, mLayout.mScale, DELTA);
        assertImage(0, 60, 75, 120);
        assertClip(0, 80, 75, 100);

        assertTrue(mLayout.layout(150, 120, 10, 3, 7.5f, 20, 0, 0, 10, 0, 0));
        assertEquals(0.5f, mLayout.mScale, DELTA);
        assertImage(0, 0, 75, 60);
    }

    public void testZeroSizedImageDoesNotDivideByZero() {
        mLayout.layout(0, 0, 0, 0, 10, 20, 0, 0, 1, 0, 0);
        assertEquals(1, mLayout.mColumns);
        assertEquals(1, mLayout.mRows);
        assertFalse(Float.isNaN(mLayout.mScale) || Float.isInfinite(mLayout.mScale));
    }

}
