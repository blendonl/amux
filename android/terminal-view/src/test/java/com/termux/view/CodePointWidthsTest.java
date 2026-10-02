package com.termux.view;

import junit.framework.TestCase;

public class CodePointWidthsTest extends TestCase {

    private final CodePointWidths mWidths = new CodePointWidths();

    public void testUnknownCodePointsHaveNoWidth() {
        assertTrue(Float.isNaN(mWidths.get(0x4E2D)));
        assertTrue(Float.isNaN(mWidths.get(0)));
        assertEquals(0, mWidths.size());
    }

    public void testStoredWidthsComeBack() {
        mWidths.put(0x4E2D, 24f);
        mWidths.put(0x1F600, 31.5f);
        mWidths.put(0, 0f);
        mWidths.put(Character.MAX_CODE_POINT, 12f);

        assertEquals(24f, mWidths.get(0x4E2D));
        assertEquals(31.5f, mWidths.get(0x1F600));
        assertEquals(0f, mWidths.get(0));
        assertEquals(12f, mWidths.get(Character.MAX_CODE_POINT));
        assertTrue(Float.isNaN(mWidths.get(0x4E2E)));
        assertEquals(4, mWidths.size());
    }

    public void testPuttingAgainReplacesTheWidth() {
        mWidths.put(0x2500, 12f);
        mWidths.put(0x2500, 13f);

        assertEquals(13f, mWidths.get(0x2500));
        assertEquals(1, mWidths.size());
    }

    public void testGrowingKeepsEveryWidth() {
        final int count = 10_000;
        for (int i = 0; i < count; i++) mWidths.put(0x4E00 + i * 7, i);

        assertEquals(count, mWidths.size());
        for (int i = 0; i < count; i++) assertEquals((float) i, mWidths.get(0x4E00 + i * 7));
        assertTrue(Float.isNaN(mWidths.get(0x4E01)));
    }

    public void testStartsOverWhenFull() {
        for (int i = 0; i < CodePointWidths.MAX_SIZE; i++) mWidths.put(0x10000 + i, 1f);
        assertEquals(CodePointWidths.MAX_SIZE, mWidths.size());

        mWidths.put(0x4E2D, 24f);

        assertEquals(1, mWidths.size());
        assertEquals(24f, mWidths.get(0x4E2D));
        assertTrue(Float.isNaN(mWidths.get(0x10000)));
    }

}
