package com.termux.terminal;

final class ApcBuffer {

    static final int MAX_LENGTH = 1024 * 1024;

    private static final int RETAINED_CAPACITY = 64 * 1024;

    private StringBuilder mBody = new StringBuilder();
    private boolean mStarted;
    private boolean mKeep;

    void start() {
        release();
        mStarted = false;
        mKeep = false;
    }

    void append(int codePoint) {
        if (!mStarted) {
            mStarted = true;
            mKeep = codePoint == 'G';
        } else if (mKeep && mBody.length() >= MAX_LENGTH) {
            mKeep = false;
            release();
        } else if (mKeep) {
            mBody.appendCodePoint(codePoint);
        }
    }

    String finishGraphicsCommand() {
        String body = mKeep ? mBody.toString() : null;
        start();
        return body;
    }

    private void release() {
        if (mBody.capacity() > RETAINED_CAPACITY) mBody = new StringBuilder();
        else mBody.setLength(0);
    }

}
