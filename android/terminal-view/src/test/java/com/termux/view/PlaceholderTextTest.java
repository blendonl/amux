package com.termux.view;

import com.termux.terminal.Placeholders;

import junit.framework.TestCase;

public class PlaceholderTextTest extends TestCase {

    private static String cell(int... diacritics) {
        StringBuilder cell = new StringBuilder().appendCodePoint(Placeholders.BASE);
        for (int index : diacritics) cell.appendCodePoint(Placeholders.diacritic(index));
        return cell.toString();
    }

    public void testTextWithoutPlaceholdersIsUnchanged() {
        String text = "plain é text\n😀";
        assertSame(text, PlaceholderText.blank(text));
        assertNull(PlaceholderText.blank(null));
    }

    public void testPlaceholderCellsBecomeSpaces() {
        assertEquals("a  b", PlaceholderText.blank("a" + cell(0, 0) + cell(0, 1, 2) + "b"));
        assertEquals(" ", PlaceholderText.blank(cell()));
    }

    public void testSupplementaryDiacriticsAreDropped() {
        assertEquals(" ", PlaceholderText.blank(cell(Placeholders.COUNT - 1, Placeholders.COUNT - 2, 0)));
    }

    public void testLineBreaksAndOtherMarksAreKept() {
        assertEquals(" \n ́x", PlaceholderText.blank(cell(1) + "\n" + cell(2) + "́x"));
    }

}
