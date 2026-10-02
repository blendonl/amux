package com.termux.terminal;

final class KittyCommand {

    private static final long INVALID = Long.MIN_VALUE;
    private static final long MAX_UNSIGNED = 0xFFFFFFFFL;

    char mAction;
    char mMedium;
    char mCompression;
    char mDeleteSelector;
    int mFormat;
    int mDataWidth;
    int mDataHeight;
    int mDataSize;
    int mDataOffset;
    int mId;
    int mNumber;
    int mPlacementId;
    int mMore;
    int mQuiet;
    int mUnicodePlaceholder;
    int mColumns;
    int mRows;
    int mX;
    int mY;
    int mWidth;
    int mHeight;
    int mCellX;
    int mCellY;
    int mCursorMovement;
    int mZIndex;
    int mParentId;
    int mParentPlacementId;
    int mParentOffsetX;
    int mParentOffsetY;
    String mPayload = "";

    static KittyCommand parse(String body) {
        KittyCommand command = new KittyCommand();
        int semicolon = body.indexOf(';');
        int controlEnd = semicolon < 0 ? body.length() : semicolon;
        if (semicolon >= 0) command.mPayload = body.substring(semicolon + 1);

        int position = 0;
        while (position < controlEnd) {
            char key = body.charAt(position);
            if (position + 1 >= controlEnd || body.charAt(position + 1) != '=') return null;
            int valueStart = position + 2;
            int valueEnd = body.indexOf(',', valueStart);
            if (valueEnd < 0 || valueEnd > controlEnd) valueEnd = controlEnd;
            if (!command.set(key, body.substring(valueStart, valueEnd))) return null;
            position = valueEnd + 1;
        }
        return command;
    }

    private boolean set(char key, String value) {
        if (key == 'a' || key == 't' || key == 'o' || key == 'd') {
            if (value.length() != 1) return false;
            char flag = value.charAt(0);
            if (key == 'a') mAction = flag;
            else if (key == 't') mMedium = flag;
            else if (key == 'o') mCompression = flag;
            else mDeleteSelector = flag;
            return true;
        }

        long number = parseNumber(value, key == 'z' || key == 'H' || key == 'V');
        if (number == INVALID) return false;
        int bits = (int) number;
        switch (key) {
            case 'f': mFormat = bits; break;
            case 's': mDataWidth = bits; break;
            case 'v': mDataHeight = bits; break;
            case 'S': mDataSize = bits; break;
            case 'O': mDataOffset = bits; break;
            case 'i': mId = bits; break;
            case 'I': mNumber = bits; break;
            case 'p': mPlacementId = bits; break;
            case 'm': mMore = bits; break;
            case 'q': mQuiet = bits; break;
            case 'U': mUnicodePlaceholder = bits; break;
            case 'c': mColumns = bits; break;
            case 'r': mRows = bits; break;
            case 'x': mX = bits; break;
            case 'y': mY = bits; break;
            case 'w': mWidth = bits; break;
            case 'h': mHeight = bits; break;
            case 'X': mCellX = bits; break;
            case 'Y': mCellY = bits; break;
            case 'C': mCursorMovement = bits; break;
            case 'z': mZIndex = bits; break;
            case 'P': mParentId = bits; break;
            case 'Q': mParentPlacementId = bits; break;
            case 'H': mParentOffsetX = bits; break;
            case 'V': mParentOffsetY = bits; break;
            default: return false;
        }
        return true;
    }

    private static long parseNumber(String value, boolean signed) {
        int length = value.length();
        boolean negative = signed && length > 0 && value.charAt(0) == '-';
        int start = negative ? 1 : 0;
        if (start == length) return INVALID;

        long result = 0;
        for (int i = start; i < length; i++) {
            char c = value.charAt(i);
            if (c < '0' || c > '9') return INVALID;
            result = result * 10 + (c - '0');
            if (result > MAX_UNSIGNED) return INVALID;
        }
        if (negative) result = -result;
        if (signed && (result < Integer.MIN_VALUE || result > Integer.MAX_VALUE)) return INVALID;
        return result;
    }

}
