ALTER TABLE invitation_links ADD COLUMN description TEXT NOT NULL DEFAULT '';

WITH RECURSIVE note_normalized(id, value, slug) AS (
  SELECT
    id,
    trim(
      replace(
        replace(
          replace(coalesce(internal_note, ''), char(13), ' '),
          char(10),
          ' '
        ),
        char(9),
        ' '
      )
    ),
    slug
  FROM invitation_links
  UNION ALL
  SELECT id, replace(value, '  ', ' '), slug
  FROM note_normalized
  WHERE instr(value, '  ') > 0
), note_final AS (
  SELECT id, coalesce(nullif(value, ''), slug) AS value
  FROM note_normalized
  WHERE instr(value, '  ') = 0
), normalized(id, value) AS (
  SELECT id, value
  FROM note_final
  UNION ALL
  SELECT id, replace(value, '  ', ' ')
  FROM normalized
  WHERE instr(value, '  ') > 0
), final AS (
  SELECT id, substr(value, 1, 120) AS description
  FROM normalized
  WHERE instr(value, '  ') = 0
)
UPDATE invitation_links
SET description = (
  SELECT final.description
  FROM final
  WHERE final.id = invitation_links.id
);
