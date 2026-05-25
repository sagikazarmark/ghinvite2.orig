ALTER TABLE invitation_links ADD COLUMN description TEXT NOT NULL DEFAULT '';

WITH RECURSIVE normalized(id, value) AS (
  SELECT
    id,
    trim(
      replace(
        replace(
          replace(coalesce(nullif(trim(internal_note), ''), slug), char(13), ' '),
          char(10),
          ' '
        ),
        char(9),
        ' '
      )
    )
  FROM invitation_links
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
