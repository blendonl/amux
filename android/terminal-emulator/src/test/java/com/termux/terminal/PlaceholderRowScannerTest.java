package com.termux.terminal;

import com.termux.terminal.PlaceholderRowScanner.Run;

import java.util.ArrayList;
import java.util.Arrays;
import java.util.List;

public class PlaceholderRowScannerTest extends TerminalTestCase {

	private static String cell(int... diacritics) {
		StringBuilder builder = new StringBuilder().appendCodePoint(Placeholders.BASE);
		for (int index : diacritics) builder.appendCodePoint(Placeholders.diacritic(index));
		return builder.toString();
	}

	private static Run runAt(int screenColumn, int count, int imageId, int imageRow, int imageColumn) {
		return new Run(screenColumn, count, imageId, imageRow, imageColumn);
	}

	private TerminalRow row(int externalRow) {
		TerminalBuffer screen = mTerminal.getScreen();
		return screen.allocateFullLineIfNecessary(screen.externalToInternalRow(externalRow));
	}

	private List<Run> runs(int externalRow, int columns) {
		List<Run> runs = new ArrayList<>();
		PlaceholderRowScanner.scan(row(externalRow), columns, (screenColumn, count, imageId, imageRow, imageColumn) ->
				runs.add(runAt(screenColumn, count, imageId, imageRow, imageColumn)));
		return runs;
	}

	private void assertRuns(int externalRow, Run... expected) {
		assertEquals(Arrays.asList(expected), runs(externalRow, mTerminal.mColumns));
	}

	public void testFullDiacriticsGiveOneRun() {
		withTerminalSized(10, 2);
		enterString("ab\033[38;2;0;0;42m" + cell(0, 0, 0) + cell(0, 1, 0) + cell(0, 2, 0) + "\033[mcd");
		assertRuns(0, runAt(2, 3, 42, 0, 0));
		enterString("\r\n\033[38;2;0;0;42m" + cell(1, 0, 0) + cell(1, 1, 0) + cell(1, 2, 0) + cell(1, 3, 0));
		assertRuns(1, runAt(0, 4, 42, 1, 0));
	}

	public void testThirdDiacriticIsTheHighByte() {
		withTerminalSized(10, 4);
		enterString("\033[38;2;18;52;86m" + cell(3, 7, 0x2a) + cell(3, 8, 0x2a));
		assertRuns(0, runAt(0, 2, 0x2a123456, 3, 7));
		enterString("\r\n\033[38;2;255;255;255m" + cell(0, 0, 0xff) + cell(0, 1, 0xff));
		assertRuns(1, runAt(0, 2, 0xffffffff, 0, 0));
		enterString("\r\n\033[38;2;0;0;0m" + cell(0, 0, 1) + cell(5, 0) + cell(0, 0, 2));
		assertRuns(2, runAt(0, 1, 0x01000000, 0, 0), runAt(2, 1, 0x02000000, 0, 0));
		enterString("\r\n\033[38;2;0;0;9m" + cell(0, 0, 1) + cell(0, 1, 2));
		assertRuns(3, runAt(0, 1, 0x01000009, 0, 0), runAt(1, 1, 0x02000009, 0, 1));
	}

	public void testCellWithoutDiacriticsContinuesTheCellToTheLeft() {
		withTerminalSized(10, 2);
		enterString("\033[38;2;0;0;7m" + cell(4, 2, 3) + cell() + cell());
		assertRuns(0, runAt(0, 3, 0x03000007, 4, 2));
		enterString("\r\n" + cell() + cell() + cell());
		assertRuns(1, runAt(0, 3, 7, 0, 0));
	}

	public void testCellWithOnlyTheRowContinuesWhenRowAndIdMatch() {
		withTerminalSized(10, 3);
		enterString("\033[38;2;0;0;7m" + cell(4, 2, 3) + cell(4) + cell(4));
		assertRuns(0, runAt(0, 3, 0x03000007, 4, 2));
		enterString("\r\n" + cell(4, 2, 3) + cell(5) + cell(5));
		assertRuns(1, runAt(0, 1, 0x03000007, 4, 2), runAt(1, 2, 7, 5, 0));
		enterString("\r\n" + cell(4, 2, 3) + "\033[38;2;0;0;8m" + cell(4));
		assertRuns(2, runAt(0, 1, 0x03000007, 4, 2), runAt(1, 1, 8, 4, 0));
	}

	public void testCellWithoutTheHighByteInheritsItWhenRowIdAndColumnMatch() {
		withTerminalSized(10, 4);
		enterString("\033[38;2;0;0;7m" + cell(4, 2, 3) + cell(4, 3) + cell(4, 4));
		assertRuns(0, runAt(0, 3, 0x03000007, 4, 2));
		enterString("\r\n" + cell(4, 2, 3) + cell(4, 9));
		assertRuns(1, runAt(0, 1, 0x03000007, 4, 2), runAt(1, 1, 7, 4, 9));
		enterString("\r\n" + cell(4, 2, 3) + cell(5, 3));
		assertRuns(2, runAt(0, 1, 0x03000007, 4, 2), runAt(1, 1, 7, 5, 3));
		enterString("\r\n" + cell(4, 2, 3) + "\033[38;2;0;0;8m" + cell(4, 3));
		assertRuns(3, runAt(0, 1, 0x03000007, 4, 2), runAt(1, 1, 8, 4, 3));
	}

	public void testInheritanceNeedsAPlaceholderDirectlyToTheLeft() {
		withTerminalSized(10, 2);
		enterString("\033[38;2;0;0;7m" + cell(3, 3, 1) + "x" + cell() + cell());
		assertRuns(0, runAt(0, 1, 0x01000007, 3, 3), runAt(2, 2, 7, 0, 0));
		enterString("\r\n" + cell(3, 3) + "中" + cell(3));
		assertRuns(1, runAt(0, 1, 7, 3, 3), runAt(3, 1, 7, 3, 0));
	}

	public void testMarksThatAreNotDiacriticsCountAsMissing() {
		withTerminalSized(10, 1);
		String unknown = "\u0301";
		String column6 = new String(Character.toChars(Placeholders.diacritic(6)));
		enterString("\033[38;2;0;0;5m" + cell(1, 4) + cell() + unknown + cell() + unknown + column6 + cell(1, 7, 0, 9));
		assertRuns(0, runAt(0, 4, 5, 1, 4));
	}

	public void testIndexedForegroundIsTheId() {
		withTerminalSized(10, 4);
		enterString("\033[38;5;42m" + cell(0, 0) + cell(0, 1));
		assertRuns(0, runAt(0, 2, 42, 0, 0));
		enterString("\r\n\033[31m" + cell(2, 5) + cell());
		assertRuns(1, runAt(0, 2, 1, 2, 5));
		enterString("\r\n\033[38;5;200m" + cell(0, 0, 0x80) + "\033[38;5;201m" + cell());
		assertRuns(2, runAt(0, 1, 0x800000c8, 0, 0), runAt(1, 1, 201, 0, 0));
		enterString("\r\n\033[38;5;9m" + cell(0, 0) + "\033[38;2;0;0;9m" + cell(0, 1));
		assertRuns(3, runAt(0, 2, 9, 0, 0));
	}

	public void testDefaultForegroundIsSkipped() {
		withTerminalSized(10, 3);
		enterString(cell(0, 0) + cell(0, 1) + cell(0, 2, 1));
		assertRuns(0);
		enterString("\r\n\033[38;2;0;0;9m" + cell(0, 0) + "\033[39m" + cell(0, 1) + "\033[38;2;0;0;9m" + cell());
		assertRuns(1, runAt(0, 1, 9, 0, 0), runAt(2, 1, 9, 0, 0));
		enterString("\r\n\033[38;2;0;0;0m" + cell(0, 0) + cell(0, 1));
		assertRuns(2);
	}

	public void testTwoImagesOnOneRow() {
		withTerminalSized(10, 2);
		enterString("\033[38;2;0;0;1m" + cell(0, 0) + cell(0, 1) + "\033[38;2;0;0;2m" + cell(0, 0) + cell(0, 1) + cell(0, 2));
		assertRuns(0, runAt(0, 2, 1, 0, 0), runAt(2, 3, 2, 0, 0));
		enterString("\r\n\033[38;2;0;0;1m" + cell(0, 0) + cell(0, 1) + cell(1, 2) + cell(1, 3) + cell(1, 7) + cell(1, 8, 1));
		assertRuns(1, runAt(0, 2, 1, 0, 0), runAt(2, 2, 1, 1, 2), runAt(4, 1, 1, 1, 7), runAt(5, 1, 0x01000001, 1, 8));
	}

	public void testPlaceholdersInTheLastColumn() {
		withTerminalSized(4, 3);
		enterString("ab\033[38;2;0;0;5m" + cell(0, 0) + cell(0, 1));
		assertCursorAt(0, 3);
		assertRuns(0, runAt(2, 2, 5, 0, 0));
		enterString(cell(0, 2));
		assertCursorAt(1, 1);
		assertRuns(0, runAt(2, 2, 5, 0, 0));
		assertRuns(1, runAt(0, 1, 5, 0, 2));
		enterString("\033[3;3H" + cell(2, 6) + cell());
		assertRuns(2, runAt(2, 2, 5, 2, 6));
	}

	public void testColumnsLimitTheScan() {
		withTerminalSized(6, 1);
		enterString("\033[38;2;0;0;5m" + cell(0, 0) + cell() + cell() + cell());
		assertEquals(Arrays.asList(runAt(0, 2, 5, 0, 0)), runs(0, 2));
		assertTrue(runs(0, 0).isEmpty());
	}

	public void testRowsWithoutPlaceholdersGiveNothing() {
		withTerminalSized(6, 4);
		enterString("hello\r\n中文\uD83D\uDE00\r\n" + new String(Character.toChars(0x10EEED)) + "\u0305x\r\n\033[38;2;0;0;5m" + cell(0, 0) + cell(0, 1));
		assertFalse(PlaceholderRowScanner.mayContainPlaceholders(row(0)));
		assertRuns(0);
		assertFalse(PlaceholderRowScanner.mayContainPlaceholders(row(1)));
		assertRuns(1);
		assertTrue(PlaceholderRowScanner.mayContainPlaceholders(row(2)));
		assertRuns(2);
		assertTrue(PlaceholderRowScanner.mayContainPlaceholders(row(3)));
		assertRuns(3, runAt(0, 2, 5, 0, 0));
		enterString("\033[4;1Hxy");
		assertFalse(PlaceholderRowScanner.mayContainPlaceholders(row(3)));
		assertRuns(3);
	}

}
