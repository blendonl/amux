package com.termux.terminal;

import junit.framework.TestCase;

public class KittyCommandTest extends TestCase {

    public void testParsesEveryKey() {
        KittyCommand command = KittyCommand.parse("a=T,t=d,o=z,d=I,f=100,s=10,v=20,S=30,O=40,i=1,I=2,p=3,m=1,q=2,U=1,c=4,r=5,"
            + "x=6,y=7,w=8,h=9,X=11,Y=12,C=1,z=-13,P=14,Q=15,H=-16,V=17;cGF5bG9hZA==");
        assertNotNull(command);
        assertEquals('T', command.mAction);
        assertEquals('d', command.mMedium);
        assertEquals('z', command.mCompression);
        assertEquals('I', command.mDeleteSelector);
        assertEquals(100, command.mFormat);
        assertEquals(10, command.mDataWidth);
        assertEquals(20, command.mDataHeight);
        assertEquals(30, command.mDataSize);
        assertEquals(40, command.mDataOffset);
        assertEquals(1, command.mId);
        assertEquals(2, command.mNumber);
        assertEquals(3, command.mPlacementId);
        assertEquals(1, command.mMore);
        assertEquals(2, command.mQuiet);
        assertEquals(1, command.mUnicodePlaceholder);
        assertEquals(4, command.mColumns);
        assertEquals(5, command.mRows);
        assertEquals(6, command.mX);
        assertEquals(7, command.mY);
        assertEquals(8, command.mWidth);
        assertEquals(9, command.mHeight);
        assertEquals(11, command.mCellX);
        assertEquals(12, command.mCellY);
        assertEquals(1, command.mCursorMovement);
        assertEquals(-13, command.mZIndex);
        assertEquals(14, command.mParentId);
        assertEquals(15, command.mParentPlacementId);
        assertEquals(-16, command.mParentOffsetX);
        assertEquals(17, command.mParentOffsetY);
        assertEquals("cGF5bG9hZA==", command.mPayload);
    }

    public void testDefaultsAreZero() {
        KittyCommand command = KittyCommand.parse("");
        assertNotNull(command);
        assertEquals(0, command.mAction);
        assertEquals(0, command.mMedium);
        assertEquals(0, command.mCompression);
        assertEquals(0, command.mDeleteSelector);
        assertEquals(0, command.mFormat);
        assertEquals(0, command.mId);
        assertEquals(0, command.mMore);
        assertEquals(0, command.mQuiet);
        assertEquals("", command.mPayload);
    }

    public void testPayload() {
        assertEquals("AAAA", KittyCommand.parse(";AAAA").mPayload);
        assertEquals("", KittyCommand.parse("i=1").mPayload);
        assertEquals("", KittyCommand.parse("i=1;").mPayload);
        assertEquals("a=b;c,d=e", KittyCommand.parse("i=1;a=b;c,d=e").mPayload);
        assertEquals(1, KittyCommand.parse("i=1;a=b;c,d=e").mId);
    }

    public void testTrailingCommaAndLastValueWins() {
        KittyCommand command = KittyCommand.parse("i=1,i=7,;AAAA");
        assertNotNull(command);
        assertEquals(7, command.mId);
        assertEquals("AAAA", command.mPayload);
    }

    public void testUnsignedRange() {
        KittyCommand command = KittyCommand.parse("i=4294967295,z=-2147483648,H=2147483647");
        assertNotNull(command);
        assertEquals(4294967295L, Integer.toUnsignedLong(command.mId));
        assertEquals(Integer.MIN_VALUE, command.mZIndex);
        assertEquals(Integer.MAX_VALUE, command.mParentOffsetX);
        assertEquals(5, KittyCommand.parse("i=0005").mId);
    }

    public void testMalformed() {
        String[] malformed = {
            "a",
            "a=",
            "a=tt",
            "=t",
            ",a=t",
            "a=t,,i=1",
            "i=",
            "i=x",
            "i=1x",
            "i=-1",
            "i=+1",
            "i=4294967296",
            "i=99999999999999999999",
            "z=-",
            "z=2147483648",
            "z=-2147483649",
            "K=1",
            "zz=1",
            "i:1",
            "i=1 ",
        };
        for (String body : malformed) {
            assertNull(body, KittyCommand.parse(body));
        }
        assertNull(KittyCommand.parse("i=1,a;AAAA"));
        assertNull(KittyCommand.parse("i=1,j=2;AAAA"));
    }

}
