-- Repository member events do not route delivery mutations. The old matching
-- facade has no production caller; organization webhook receipts remain intact.
DROP TABLE webhook_receipts;
