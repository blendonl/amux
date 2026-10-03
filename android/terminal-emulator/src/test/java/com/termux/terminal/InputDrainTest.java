package com.termux.terminal;

import junit.framework.TestCase;

import java.io.ByteArrayOutputStream;
import java.util.Arrays;
import java.util.concurrent.LinkedBlockingQueue;
import java.util.concurrent.TimeUnit;
import java.util.concurrent.atomic.AtomicInteger;

public class InputDrainTest extends TestCase {

    private static final class Collected implements InputDrain.Consumer {
        final ByteArrayOutputStream mBytes = new ByteArrayOutputStream();
        int mCalls;

        @Override
        public void accept(byte[] buffer, int length) {
            mBytes.write(buffer, 0, length);
            mCalls++;
        }
    }

    private static byte[] bytes(int count, int seed) {
        final byte[] bytes = new byte[count];
        for (int i = 0; i < count; i++) bytes[i] = (byte) (seed + i * 31);
        return bytes;
    }

    public void testOnlyOnePostIsPendingUntilTheDrain() {
        final InputDrain drain = new InputDrain(new ByteQueue(64), 16, 1024);

        assertTrue(drain.shouldPost());
        assertFalse(drain.shouldPost());
        assertFalse(drain.shouldPost());
        drain.drain(new Collected());
        assertTrue(drain.shouldPost());
    }

    public void testDrainHandsOverEverythingQueuedInOrder() {
        final ByteQueue queue = new ByteQueue(4096);
        final InputDrain drain = new InputDrain(queue, 1000, 64 * 1024);
        final byte[] written = bytes(3000, 7);
        queue.write(written, 0, written.length);
        final Collected collected = new Collected();

        assertEquals(3000, drain.drain(collected));
        assertTrue(Arrays.equals(written, collected.mBytes.toByteArray()));
        assertEquals(3, collected.mCalls);
    }

    public void testDrainStopsAtTheLimitAndLeavesTheRest() {
        final ByteQueue queue = new ByteQueue(1024);
        final InputDrain drain = new InputDrain(queue, 100, 250);
        final byte[] written = bytes(1000, 3);
        queue.write(written, 0, written.length);
        final Collected collected = new Collected();

        assertEquals(300, drain.drain(collected));
        assertEquals(300, drain.drain(collected));
        assertEquals(300, drain.drain(collected));
        assertEquals(100, drain.drain(collected));
        assertEquals(0, drain.drain(collected));
        assertTrue(Arrays.equals(written, collected.mBytes.toByteArray()));
    }

    public void testAnEmptyOrClosedQueueDrainsNothing() {
        final ByteQueue queue = new ByteQueue(64);
        final InputDrain drain = new InputDrain(queue, 16, 1024);
        final Collected collected = new Collected();

        assertEquals(0, drain.drain(collected));
        queue.write(new byte[]{1, 2, 3}, 0, 3);
        queue.close();
        assertEquals(0, drain.drain(collected));
        assertEquals(0, collected.mCalls);
    }

    public void testEveryByteArrivesWithOneMessagePendingAtATime() throws Exception {
        final int limit = 4096;
        final ByteQueue queue = new ByteQueue(1024);
        final InputDrain drain = new InputDrain(queue, 512, limit);
        final LinkedBlockingQueue<Integer> messages = new LinkedBlockingQueue<>();
        final AtomicInteger mostPending = new AtomicInteger();
        final byte[] written = bytes(200_000, 11);

        final Thread reader = new Thread(() -> {
            for (int offset = 0; offset < written.length; offset += 700) {
                final int length = Math.min(700, written.length - offset);
                queue.write(written, offset, length);
                if (drain.shouldPost()) {
                    messages.add(1);
                    mostPending.accumulateAndGet(messages.size(), Math::max);
                }
            }
        });
        reader.start();

        final Collected collected = new Collected();
        while (collected.mBytes.size() < written.length) {
            final Integer message = messages.poll(10, TimeUnit.SECONDS);
            assertNotNull("bytes are left without a message to drain them", message);
            final int drained = drain.drain(collected);
            if (drained >= limit && drain.shouldPost()) {
                messages.add(1);
                mostPending.accumulateAndGet(messages.size(), Math::max);
            }
        }
        reader.join();

        assertTrue(Arrays.equals(written, collected.mBytes.toByteArray()));
        assertTrue("messages pending at once: " + mostPending.get(), mostPending.get() <= 1);
        assertTrue(messages.size() <= 1);
    }

}
