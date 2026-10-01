package com.termux.view;

import com.termux.terminal.Placeholders;

public final class PlaceholderText {

    private PlaceholderText() {
    }

    public static String blank(String text) {
        if (text == null || text.indexOf(Placeholders.HIGH_SURROGATE) < 0) return text;
        final StringBuilder result = new StringBuilder(text.length());
        for (int index = 0; index < text.length(); ) {
            final int codePoint = text.codePointAt(index);
            index += Character.charCount(codePoint);
            if (codePoint != Placeholders.BASE) {
                result.appendCodePoint(codePoint);
                continue;
            }
            result.append(' ');
            while (index < text.length()) {
                final int mark = text.codePointAt(index);
                if (Placeholders.indexOf(mark) < 0) break;
                index += Character.charCount(mark);
            }
        }
        return result.toString();
    }

}
