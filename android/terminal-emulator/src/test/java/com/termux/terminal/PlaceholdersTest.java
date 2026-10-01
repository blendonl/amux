package com.termux.terminal;

import junit.framework.TestCase;

import java.util.HashSet;
import java.util.Set;

public class PlaceholdersTest extends TestCase {

	public void testTableHasKittysDiacritics() {
		assertEquals(297, Placeholders.COUNT);
		assertEquals(0x0305, Placeholders.diacritic(0));
		assertEquals(0x030D, Placeholders.diacritic(1));
		assertEquals(0x030E, Placeholders.diacritic(2));
		assertEquals(0xFE26, Placeholders.diacritic(282));
		assertEquals(0x10A0F, Placeholders.diacritic(283));
		assertEquals(0x1D244, Placeholders.diacritic(296));
		try {
			Placeholders.diacritic(297);
			fail();
		} catch (ArrayIndexOutOfBoundsException expected) {
		}
	}

	public void testTableIsStrictlyAscending() {
		Set<Integer> seen = new HashSet<>();
		for (int i = 0; i < Placeholders.COUNT; i++) {
			assertTrue(seen.add(Placeholders.diacritic(i)));
			if (i > 0) assertTrue(Placeholders.diacritic(i - 1) < Placeholders.diacritic(i));
		}
	}

	public void testEveryDiacriticHasZeroWidth() {
		for (int i = 0; i < Placeholders.COUNT; i++)
			assertEquals("index=" + i, 0, WcWidth.width(Placeholders.diacritic(i)));
	}

	public void testBaseHasWidthOneAndItsSurrogates() {
		assertEquals(1, WcWidth.width(Placeholders.BASE));
		assertEquals(Character.highSurrogate(Placeholders.BASE), Placeholders.HIGH_SURROGATE);
		assertEquals(Character.lowSurrogate(Placeholders.BASE), Placeholders.LOW_SURROGATE);
		assertEquals(Character.PRIVATE_USE, Character.getType(Placeholders.BASE));
	}

	public void testIndexOfRoundTrips() {
		for (int i = 0; i < Placeholders.COUNT; i++)
			assertEquals(i, Placeholders.indexOf(Placeholders.diacritic(i)));
	}

	public void testSupplementaryDiacritics() {
		int[] expected = {0x10A0F, 0x10A38, 0x1D185, 0x1D186, 0x1D187, 0x1D188, 0x1D189, 0x1D1AA, 0x1D1AB, 0x1D1AC, 0x1D1AD,
				0x1D242, 0x1D243, 0x1D244};
		for (int i = 0; i < expected.length; i++)
			assertEquals(283 + i, Placeholders.indexOf(expected[i]));
	}

	public void testIndexOfEverythingElse() {
		Set<Integer> diacritics = new HashSet<>();
		for (int i = 0; i < Placeholders.COUNT; i++) diacritics.add(Placeholders.diacritic(i));
		for (int codePoint = 0; codePoint <= 0x1FFFF; codePoint++)
			if (!diacritics.contains(codePoint)) assertEquals("codePoint=" + Integer.toHexString(codePoint), -1, Placeholders.indexOf(codePoint));
		assertEquals(-1, Placeholders.indexOf(Placeholders.BASE));
		assertEquals(-1, Placeholders.indexOf(Character.MAX_CODE_POINT));
		assertEquals(-1, Placeholders.indexOf(-1));
	}

}
