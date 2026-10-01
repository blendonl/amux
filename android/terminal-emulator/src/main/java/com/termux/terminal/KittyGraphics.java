package com.termux.terminal;

import java.io.ByteArrayOutputStream;
import java.util.ArrayList;
import java.util.Arrays;
import java.util.Base64;
import java.util.List;
import java.util.function.Predicate;
import java.util.zip.DataFormatException;
import java.util.zip.Inflater;

final class KittyGraphics {

    static final int MAX_DIMENSION = 10000;
    static final long MAX_INFLATED_PNG = 400_000_000L;

    private static final byte[] PNG_SIGNATURE = {(byte) 0x89, 'P', 'N', 'G', '\r', '\n', 0x1A, '\n'};
    private static final int PNG_HEADER_LENGTH = 24;
    private static final int INFLATE_BUFFER_LENGTH = 64 * 1024;

    private static final class Failure extends Exception {
        Failure(String code, String message) {
            super(code + ":" + message);
        }
    }

    private static final class Transmission {
        final KittyCommand mStart;
        final char mAction;
        ByteArrayOutputStream mData = new ByteArrayOutputStream();
        int mQuiet;
        String mCarry = "";
        boolean mFailed;

        Transmission(KittyCommand start, char action) {
            mStart = start;
            mAction = action;
            mQuiet = start.mQuiet;
        }
    }

    private final ImageStore mStore;
    private final TerminalOutput mOutput;
    private Transmission mLoading;

    KittyGraphics(ImageStore store, TerminalOutput output) {
        mStore = store;
        mOutput = output;
    }

    void reset() {
        mLoading = null;
        mStore.clear();
    }

    void handle(String body) {
        KittyCommand command = KittyCommand.parse(body);
        if (command == null) return;
        char action = command.mAction == 0 ? 't' : command.mAction;
        switch (action) {
            case 't':
            case 'T':
            case 'q':
                transmit(command, action);
                break;
            case 'p':
                put(command);
                break;
            case 'd':
                delete(command);
                break;
            default:
                reject(command, action);
                break;
        }
    }

    private void reject(KittyCommand command, char action) {
        Transmission rejected = new Transmission(command, action);
        if (command.mMore != 0) mLoading = rejected;
        fail(rejected, new Failure("EINVAL", "Unsupported action: " + action));
    }

    private void transmit(KittyCommand command, char action) {
        Transmission transmission = mLoading;
        if (transmission == null) {
            transmission = new Transmission(command, action);
            try {
                checkTransmission(command, action);
            } catch (Failure failure) {
                fail(transmission, failure);
            }
        } else if (command.mQuiet != 0) {
            transmission.mQuiet = command.mQuiet;
        }

        boolean last = command.mMore == 0;
        mLoading = last ? null : transmission;
        if (transmission.mFailed) return;
        try {
            receive(transmission, command.mPayload, last);
            if (last) finish(transmission);
        } catch (Failure failure) {
            fail(transmission, failure);
        }
    }

    private void checkTransmission(KittyCommand command, char action) throws Failure {
        if (command.mId != 0 && command.mNumber != 0)
            throw new Failure("EINVAL", "Must not specify both image id and image number");
        char medium = command.mMedium == 0 ? 'd' : command.mMedium;
        if (medium != 'd') throw new Failure("EINVAL", "Unsupported transmission medium: " + medium);
        if (command.mCompression != 0 && command.mCompression != 'z')
            throw new Failure("EINVAL", "Unknown image compression: " + command.mCompression);
        if (action == 'T') checkPlacement(command);

        int format = formatOf(command);
        if (format == ImageStore.FORMAT_PNG) return;
        if (format != ImageStore.FORMAT_RGB && format != ImageStore.FORMAT_RGBA)
            throw new Failure("EINVAL", "Unknown image format: " + Integer.toUnsignedString(format));
        if (command.mDataWidth == 0 || command.mDataHeight == 0) throw new Failure("EINVAL", "Zero width/height not allowed");
        if (tooLarge(command.mDataWidth) || tooLarge(command.mDataHeight)) throw new Failure("EINVAL", "Image too large");
        if (command.mCompression == 0 && rawSize(command) > mStore.getCapacity()) throw new Failure("EFBIG", "Image too large");
    }

    private static void checkPlacement(KittyCommand command) throws Failure {
        if (command.mUnicodePlaceholder == 0) throw new Failure("EINVAL", "Only virtual placements are supported");
        if (tooLarge(command.mColumns) || tooLarge(command.mRows)) throw new Failure("EINVAL", "Placement too large");
    }

    private void receive(Transmission transmission, String payload, boolean last) throws Failure {
        String text = transmission.mCarry.isEmpty() ? payload : transmission.mCarry + payload;
        int usable = last ? text.length() : text.length() - text.length() % 4;
        transmission.mCarry = text.substring(usable);
        if (usable == 0) return;

        byte[] bytes;
        try {
            bytes = Base64.getDecoder().decode(text.substring(0, usable));
        } catch (IllegalArgumentException e) {
            throw new Failure("EINVAL", "Invalid base64 data");
        }
        if (transmission.mData.size() + (long) bytes.length > mStore.getCapacity()) throw new Failure("EFBIG", "Too much data");
        transmission.mData.write(bytes, 0, bytes.length);
    }

    private void finish(Transmission transmission) throws Failure {
        KittyCommand start = transmission.mStart;
        byte[] data = transmission.mData.toByteArray();
        int format = formatOf(start);
        boolean compressed = start.mCompression == 'z';
        int width;
        int height;
        if (format == ImageStore.FORMAT_PNG) {
            byte[] header = compressed ? inflatePngHeader(data) : data;
            checkPngHeader(header);
            width = readInt(header, 16);
            height = readInt(header, 20);
            if (width == 0 || height == 0) throw new Failure("EBADPNG", "Zero width/height in PNG header");
            if (tooLarge(width) || tooLarge(height)) throw new Failure("EINVAL", "Image too large");
        } else {
            width = start.mDataWidth;
            height = start.mDataHeight;
            long expected = rawSize(start);
            if (compressed) {
                if (inflate(data, null, expected) != expected)
                    throw new Failure("EINVAL", "Image data size post inflation does not match expected size");
            } else if (data.length < expected) {
                throw new Failure("ENODATA", "Insufficient image data: " + data.length + " < " + expected);
            } else if (data.length > expected) {
                data = Arrays.copyOf(data, (int) expected);
            }
        }

        if (transmission.mAction == 'q') {
            reply(transmission, start.mId, null);
            return;
        }
        if (start.mId == 0 && start.mNumber == 0) return;
        int id = start.mId != 0 ? start.mId : freeId();
        ImageStore.Image image = mStore.put(id, start.mNumber, width, height, format, compressed, data);
        if (transmission.mAction == 'T') mStore.place(image, start.mPlacementId, start.mColumns, start.mRows);
        reply(transmission, id, null);
    }

    private void fail(Transmission transmission, Failure failure) {
        transmission.mFailed = true;
        transmission.mData = null;
        reply(transmission, transmission.mStart.mId, failure.getMessage());
    }

    private void put(KittyCommand command) {
        if (command.mId == 0 && command.mNumber == 0) return;
        ImageStore.Image image = command.mId != 0 ? mStore.get(command.mId) : mStore.findByNumber(command.mNumber);
        String response = null;
        try {
            if (image == null) throw new Failure("ENOENT", "Put command refers to non-existent image with id: "
                + Integer.toUnsignedString(command.mId) + " and number: " + Integer.toUnsignedString(command.mNumber));
            checkPlacement(command);
            mStore.place(image, command.mPlacementId, command.mColumns, command.mRows);
        } catch (Failure failure) {
            response = failure.getMessage();
        }
        int id = image == null ? command.mId : image.mId;
        reply(id, command.mNumber, command.mPlacementId, command.mQuiet, response);
    }

    private void delete(KittyCommand command) {
        char selector = command.mDeleteSelector == 0 ? 'a' : command.mDeleteSelector;
        boolean free = Character.isUpperCase(selector);
        switch (Character.toLowerCase(selector)) {
            case 'i':
                unplace(mStore.get(command.mId), command.mPlacementId, free);
                break;
            case 'n':
                unplace(mStore.findByNumber(command.mNumber), command.mPlacementId, free);
                break;
            case 'r':
                for (ImageStore.Image image : select(candidate -> inRange(candidate.mId, command.mX, command.mY)))
                    unplace(image, 0, free);
                break;
            case 'a':
                if (free) {
                    for (ImageStore.Image image : select(candidate -> !candidate.hasPlacements())) mStore.remove(image.mId);
                }
                break;
            default:
                break;
        }
    }

    private void unplace(ImageStore.Image image, int placementId, boolean free) {
        if (image == null) return;
        mStore.unplace(image, placementId);
        if (free && !image.hasPlacements()) mStore.remove(image.mId);
    }

    private List<ImageStore.Image> select(Predicate<ImageStore.Image> filter) {
        List<ImageStore.Image> selected = new ArrayList<>();
        for (ImageStore.Image image : mStore.getImages()) {
            if (filter.test(image)) selected.add(image);
        }
        return selected;
    }

    private int freeId() {
        int id = 1;
        while (mStore.get(id) != null) id++;
        return id;
    }

    private void reply(Transmission transmission, int id, String response) {
        KittyCommand start = transmission.mStart;
        if (transmission.mAction == 'q') reply(id, 0, 0, transmission.mQuiet, response);
        else reply(id, start.mNumber, start.mPlacementId, transmission.mQuiet, response);
    }

    private void reply(int id, int number, int placementId, int quiet, String response) {
        boolean ok = response == null;
        if (quiet != 0 && (ok || quiet != 1)) return;
        if (id == 0 && number == 0) return;
        StringBuilder reply = new StringBuilder("\033_G");
        int keysStart = reply.length();
        appendKey(reply, keysStart, 'i', id);
        appendKey(reply, keysStart, 'I', number);
        appendKey(reply, keysStart, 'p', placementId);
        reply.append(';').append(ok ? "OK" : response).append("\033\\");
        mOutput.write(reply.toString());
    }

    private static void appendKey(StringBuilder reply, int keysStart, char key, int value) {
        if (value == 0) return;
        if (reply.length() > keysStart) reply.append(',');
        reply.append(key).append('=').append(Integer.toUnsignedString(value));
    }

    private static int formatOf(KittyCommand command) {
        return command.mFormat == 0 ? ImageStore.FORMAT_RGBA : command.mFormat;
    }

    private static long rawSize(KittyCommand command) {
        return Integer.toUnsignedLong(command.mDataWidth) * Integer.toUnsignedLong(command.mDataHeight) * (formatOf(command) / 8);
    }

    private static boolean tooLarge(int value) {
        return Integer.toUnsignedLong(value) > MAX_DIMENSION;
    }

    private static boolean inRange(int id, int first, int last) {
        return Integer.compareUnsigned(id, first) >= 0 && Integer.compareUnsigned(id, last) <= 0;
    }

    private static void checkPngHeader(byte[] header) throws Failure {
        if (header.length < PNG_HEADER_LENGTH) throw new Failure("EBADPNG", "Not a PNG image");
        for (int i = 0; i < PNG_SIGNATURE.length; i++) {
            if (header[i] != PNG_SIGNATURE[i]) throw new Failure("EBADPNG", "Not a PNG image");
        }
        if (header[12] != 'I' || header[13] != 'H' || header[14] != 'D' || header[15] != 'R')
            throw new Failure("EBADPNG", "Missing PNG IHDR chunk");
    }

    private static int readInt(byte[] bytes, int offset) {
        return (bytes[offset] & 0xFF) << 24 | (bytes[offset + 1] & 0xFF) << 16 | (bytes[offset + 2] & 0xFF) << 8 | (bytes[offset + 3] & 0xFF);
    }

    private static byte[] inflatePngHeader(byte[] data) throws Failure {
        byte[] header = new byte[PNG_HEADER_LENGTH];
        long total = inflate(data, header, MAX_INFLATED_PNG);
        if (total > MAX_INFLATED_PNG) throw new Failure("EFBIG", "Too much data");
        return total < header.length ? Arrays.copyOf(header, (int) total) : header;
    }

    private static long inflate(byte[] data, byte[] header, long limit) throws Failure {
        Inflater inflater = new Inflater();
        try {
            inflater.setInput(data);
            byte[] buffer = new byte[INFLATE_BUFFER_LENGTH];
            long total = 0;
            while (!inflater.finished() && total <= limit) {
                int count = inflater.inflate(buffer);
                if (count == 0 && (inflater.needsInput() || inflater.needsDictionary()))
                    throw new Failure("EINVAL", "Truncated compressed image data");
                if (header != null && total < header.length)
                    System.arraycopy(buffer, 0, header, (int) total, (int) Math.min(count, header.length - total));
                total += count;
            }
            return total;
        } catch (DataFormatException e) {
            throw new Failure("EINVAL", "Failed to inflate image data");
        } finally {
            inflater.end();
        }
    }

}
