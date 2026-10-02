package com.termux.terminal;

import java.util.concurrent.atomic.AtomicBoolean;

final class InputDrain {

    interface Consumer {
        void accept(byte[] buffer, int length);
    }

    private final ByteQueue mQueue;
    private final byte[] mBuffer;
    private final int mLimit;
    private final AtomicBoolean mPosted = new AtomicBoolean();

    InputDrain(ByteQueue queue, int bufferSize, int limit) {
        mQueue = queue;
        mBuffer = new byte[bufferSize];
        mLimit = limit;
    }

    boolean shouldPost() {
        return mPosted.compareAndSet(false, true);
    }

    int drain(Consumer consumer) {
        mPosted.set(false);
        int drained = 0;
        while (drained < mLimit) {
            final int read = mQueue.read(mBuffer, false);
            if (read <= 0) break;
            consumer.accept(mBuffer, read);
            drained += read;
        }
        return drained;
    }

}
