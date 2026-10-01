package com.termux.terminal;

public final class PlaceholderRowScanner {

    public interface RunConsumer {
        void accept(int screenColumn, int count, int imageId, int imageRow, int imageColumn);
    }

    public static final class Run {
        public final int screenColumn;
        public final int count;
        public final int imageId;
        public final int imageRow;
        public final int imageColumn;

        public Run(int screenColumn, int count, int imageId, int imageRow, int imageColumn) {
            this.screenColumn = screenColumn;
            this.count = count;
            this.imageId = imageId;
            this.imageRow = imageRow;
            this.imageColumn = imageColumn;
        }

        @Override
        public boolean equals(Object o) {
            if (!(o instanceof Run)) return false;
            Run other = (Run) o;
            return screenColumn == other.screenColumn && count == other.count && imageId == other.imageId
                && imageRow == other.imageRow && imageColumn == other.imageColumn;
        }

        @Override
        public int hashCode() {
            int result = screenColumn;
            result = 31 * result + count;
            result = 31 * result + imageId;
            result = 31 * result + imageRow;
            return 31 * result + imageColumn;
        }

        @Override
        public String toString() {
            return "Run[screenColumn=" + screenColumn + ", count=" + count + ", imageId=0x" + Integer.toHexString(imageId)
                + ", imageRow=" + imageRow + ", imageColumn=" + imageColumn + "]";
        }
    }

    private static final int NO_ID = -1;
    private static final int MISSING = -1;
    private static final int TRUECOLOR = 0xff000000;

    private PlaceholderRowScanner() {
    }

    public static boolean mayContainPlaceholders(TerminalRow row) {
        if (!row.mHasNonOneWidthOrSurrogateChars) return false;
        final char[] text = row.mText;
        for (int index = 0, used = row.getSpaceUsed(); index < used; index++)
            if (text[index] == Placeholders.HIGH_SURROGATE) return true;
        return false;
    }

    private static int imageIdLowBits(long style) {
        int color = TextStyle.decodeForeColor(style);
        if ((color & TRUECOLOR) == TRUECOLOR) return color & 0x00ffffff;
        return color < TextStyle.COLOR_INDEX_FOREGROUND ? color : NO_ID;
    }

    public static void scan(TerminalRow row, int columns, RunConsumer out) {
        if (!mayContainPlaceholders(row)) return;
        final char[] text = row.mText;
        final int used = row.getSpaceUsed();

        int runScreenColumn = 0, runCount = 0, runId = 0, runRow = 0, runColumn = 0;
        boolean leftIsPlaceholder = false;
        int leftLowBits = 0, leftRow = 0, leftColumn = 0, leftHighByte = 0;

        int column = 0;
        int index = 0;
        while (index < used && column < columns) {
            int codePoint = codePointAt(text, index, used);
            index += Character.charCount(codePoint);
            int width = WcWidth.width(codePoint);
            if (width <= 0) continue;

            int lowBits = codePoint == Placeholders.BASE ? imageIdLowBits(row.getStyle(column)) : NO_ID;
            if (lowBits == NO_ID) {
                if (runCount > 0) emit(out, runScreenColumn, runCount, runId, runRow, runColumn);
                runCount = 0;
                leftIsPlaceholder = false;
                column += width;
                continue;
            }

            int imageRow = MISSING, imageColumn = MISSING, highByte = MISSING;
            for (int mark = 0; index < used; mark++) {
                int markCodePoint = codePointAt(text, index, used);
                int diacritic = Placeholders.indexOf(markCodePoint);
                if (diacritic < 0 && WcWidth.width(markCodePoint) > 0) break;
                index += Character.charCount(markCodePoint);
                if (mark == 0) imageRow = diacritic;
                else if (mark == 1) imageColumn = diacritic;
                else if (mark == 2) highByte = diacritic;
            }

            boolean continuesLeft = leftIsPlaceholder && lowBits == leftLowBits
                && (imageRow == MISSING || imageRow == leftRow)
                && (imageColumn == MISSING || imageColumn == leftColumn + 1)
                && (highByte == MISSING || highByte == leftHighByte);
            if (continuesLeft) {
                imageRow = leftRow;
                imageColumn = leftColumn + 1;
                highByte = leftHighByte;
                runCount++;
            } else {
                if (imageRow == MISSING) imageRow = 0;
                if (imageColumn == MISSING) imageColumn = 0;
                if (highByte == MISSING) highByte = 0;
                if (runCount > 0) emit(out, runScreenColumn, runCount, runId, runRow, runColumn);
                runScreenColumn = column;
                runCount = 1;
                runId = (highByte << 24) | lowBits;
                runRow = imageRow;
                runColumn = imageColumn;
            }

            leftIsPlaceholder = true;
            leftLowBits = lowBits;
            leftRow = imageRow;
            leftColumn = imageColumn;
            leftHighByte = highByte;
            column += width;
        }
        if (runCount > 0) emit(out, runScreenColumn, runCount, runId, runRow, runColumn);
    }

    private static void emit(RunConsumer out, int screenColumn, int count, int imageId, int imageRow, int imageColumn) {
        if (imageId != 0) out.accept(screenColumn, count, imageId, imageRow, imageColumn);
    }

    private static int codePointAt(char[] text, int index, int used) {
        char c = text[index];
        if (Character.isHighSurrogate(c) && index + 1 < used) {
            char low = text[index + 1];
            if (Character.isLowSurrogate(low)) return Character.toCodePoint(c, low);
        }
        return c;
    }

}
