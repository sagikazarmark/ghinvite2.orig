-- Existing nullable columns become immutable, validated domain guardrails and evidence.
-- NULL link rules remain equivalent to [] for earlier unrestricted projections.
-- Rust applies exact timestamp/IDNA validation; SQL adds raw-write shape and binding guards.

CREATE TRIGGER link_domain_rules_insert BEFORE INSERT ON invitation_links
WHEN CASE WHEN NEW.email_domains_json IS NULL THEN 0
 WHEN NOT json_valid(NEW.email_domains_json) THEN 1
 WHEN json_type(NEW.email_domains_json)!='array' THEN 1
 WHEN json_array_length(NEW.email_domains_json)>32 THEN 1
 WHEN (SELECT count(*) FROM json_each(NEW.email_domains_json))!=(SELECT count(DISTINCT value) FROM json_each(NEW.email_domains_json)) THEN 1
 ELSE EXISTS(SELECT 1 FROM json_each(NEW.email_domains_json) r WHERE
   r.type!='object' OR (SELECT count(*) FROM json_each(r.value))!=1 OR
   NOT EXISTS(SELECT 1 FROM json_each(r.value) v WHERE v.key IN ('exact','subdomains')
     AND v.type='text' AND length(v.value) BETWEEN 1 AND 253
     AND v.value=lower(v.value) AND v.value NOT GLOB '*[^a-z0-9.-]*'
     AND substr(v.value,1,1) NOT IN ('.','-') AND substr(v.value,-1) NOT IN ('.','-')
     AND instr(v.value,'..')=0 AND instr(v.value,'.-')=0 AND instr(v.value,'-.')=0)) END
BEGIN SELECT RAISE(ABORT,'projection_invariant: invalid domain data'); END;

CREATE TRIGGER link_domain_rules_update BEFORE UPDATE ON invitation_links
WHEN CASE WHEN NEW.email_domains_json IS NULL THEN 0
 WHEN NOT json_valid(NEW.email_domains_json) THEN 1
 WHEN json_type(NEW.email_domains_json)!='array' THEN 1
 WHEN json_array_length(NEW.email_domains_json)>32 THEN 1
 WHEN (SELECT count(*) FROM json_each(NEW.email_domains_json))!=(SELECT count(DISTINCT value) FROM json_each(NEW.email_domains_json)) THEN 1
 ELSE EXISTS(SELECT 1 FROM json_each(NEW.email_domains_json) r WHERE
   r.type!='object' OR (SELECT count(*) FROM json_each(r.value))!=1 OR
   NOT EXISTS(SELECT 1 FROM json_each(r.value) v WHERE v.key IN ('exact','subdomains')
     AND v.type='text' AND length(v.value) BETWEEN 1 AND 253
     AND v.value=lower(v.value) AND v.value NOT GLOB '*[^a-z0-9.-]*'
     AND substr(v.value,1,1) NOT IN ('.','-') AND substr(v.value,-1) NOT IN ('.','-')
     AND instr(v.value,'..')=0 AND instr(v.value,'.-')=0 AND instr(v.value,'-.')=0)) END
BEGIN SELECT RAISE(ABORT,'projection_invariant: invalid domain data'); END;

CREATE TRIGGER request_domain_evidence_insert BEFORE INSERT ON invitation_requests
WHEN CASE WHEN NEW.domain_evidence_json IS NULL THEN 0
 WHEN NOT json_valid(NEW.domain_evidence_json) THEN 1
 WHEN json_type(NEW.domain_evidence_json)!='object' THEN 1
 ELSE json_extract(NEW.domain_evidence_json,'$.link_id') IS NOT NEW.invitation_link_id
   OR json_type(NEW.domain_evidence_json,'$.matched_domains') IS NOT 'array'
   OR json_array_length(NEW.domain_evidence_json,'$.matched_domains')=0
   OR json_type(NEW.domain_evidence_json,'$.checked_at') IS NOT 'text'
   OR julianday(json_extract(NEW.domain_evidence_json,'$.checked_at')) IS NULL
   OR julianday(json_extract(NEW.domain_evidence_json,'$.checked_at'))>julianday(NEW.created_at)
   OR julianday(NEW.created_at)-julianday(json_extract(NEW.domain_evidence_json,'$.checked_at'))>(600.001/86400.0)
   OR (SELECT count(*) FROM json_each(NEW.domain_evidence_json))!=3
   OR EXISTS(SELECT 1 FROM json_each(NEW.domain_evidence_json,'$.matched_domains') d WHERE
      d.type!='text' OR length(d.value) NOT BETWEEN 1 AND 253 OR d.value!=lower(d.value)
      OR d.value GLOB '*[^a-z0-9.-]*' OR d.value='users.noreply.github.com'
      OR substr(d.value,1,1) IN ('.','-') OR substr(d.value,-1) IN ('.','-')
      OR instr(d.value,'..')>0 OR instr(d.value,'.-')>0 OR instr(d.value,'-.')>0)
 END
BEGIN SELECT RAISE(ABORT,'projection_invariant: invalid domain data'); END;

CREATE TRIGGER request_domain_evidence_update BEFORE UPDATE ON invitation_requests
WHEN CASE WHEN NEW.domain_evidence_json IS NULL THEN 0
 WHEN NOT json_valid(NEW.domain_evidence_json) THEN 1
 WHEN json_type(NEW.domain_evidence_json)!='object' THEN 1
 ELSE json_extract(NEW.domain_evidence_json,'$.link_id') IS NOT NEW.invitation_link_id
   OR json_type(NEW.domain_evidence_json,'$.matched_domains') IS NOT 'array'
   OR json_array_length(NEW.domain_evidence_json,'$.matched_domains')=0
   OR json_type(NEW.domain_evidence_json,'$.checked_at') IS NOT 'text'
   OR julianday(json_extract(NEW.domain_evidence_json,'$.checked_at')) IS NULL
   OR julianday(json_extract(NEW.domain_evidence_json,'$.checked_at'))>julianday(NEW.created_at)
   OR julianday(NEW.created_at)-julianday(json_extract(NEW.domain_evidence_json,'$.checked_at'))>(600.001/86400.0)
   OR (SELECT count(*) FROM json_each(NEW.domain_evidence_json))!=3
   OR EXISTS(SELECT 1 FROM json_each(NEW.domain_evidence_json,'$.matched_domains') d WHERE
      d.type!='text' OR length(d.value) NOT BETWEEN 1 AND 253 OR d.value!=lower(d.value)
      OR d.value GLOB '*[^a-z0-9.-]*' OR d.value='users.noreply.github.com'
      OR substr(d.value,1,1) IN ('.','-') OR substr(d.value,-1) IN ('.','-')
      OR instr(d.value,'..')>0 OR instr(d.value,'.-')>0 OR instr(d.value,'-.')>0)
 END
BEGIN SELECT RAISE(ABORT,'projection_invariant: invalid domain data'); END;

CREATE TRIGGER link_domain_rules_immutable BEFORE UPDATE ON invitation_links
WHEN COALESCE(OLD.email_domains_json,'[]') IS NOT COALESCE(NEW.email_domains_json,'[]')
BEGIN SELECT RAISE(ABORT,'projection_invariant: immutable domain rules'); END;

CREATE TRIGGER request_domain_evidence_immutable BEFORE UPDATE ON invitation_requests
WHEN OLD.domain_evidence_json IS NOT NEW.domain_evidence_json
BEGIN SELECT RAISE(ABORT,'projection_invariant: immutable domain evidence'); END;

CREATE TRIGGER request_domain_parent_insert BEFORE INSERT ON invitation_requests
WHEN CASE WHEN NEW.domain_evidence_json IS NULL OR json_valid(NEW.domain_evidence_json) THEN EXISTS(SELECT 1 FROM invitation_links l WHERE l.id=NEW.invitation_link_id AND (
 (json_array_length(COALESCE(l.email_domains_json,'[]'))=0) IS NOT (NEW.domain_evidence_json IS NULL)
 OR EXISTS(SELECT 1 FROM json_each(NEW.domain_evidence_json,'$.matched_domains') d
   WHERE NOT EXISTS(SELECT 1 FROM json_each(COALESCE(l.email_domains_json,'[]')) r
     WHERE d.value=json_extract(r.value,'$.exact') OR
       (length(d.value)>length(json_extract(r.value,'$.subdomains'))+1
        AND substr(d.value,-length(json_extract(r.value,'$.subdomains'))-1)='.'||json_extract(r.value,'$.subdomains')))))) ELSE 0 END
BEGIN SELECT RAISE(ABORT,'projection_invariant: unmatched domain evidence'); END;

CREATE TRIGGER request_domain_parent_update BEFORE UPDATE ON invitation_requests
WHEN CASE WHEN NEW.domain_evidence_json IS NULL OR json_valid(NEW.domain_evidence_json) THEN EXISTS(SELECT 1 FROM invitation_links l WHERE l.id=NEW.invitation_link_id AND (
 (json_array_length(COALESCE(l.email_domains_json,'[]'))=0) IS NOT (NEW.domain_evidence_json IS NULL)
 OR EXISTS(SELECT 1 FROM json_each(NEW.domain_evidence_json,'$.matched_domains') d
   WHERE NOT EXISTS(SELECT 1 FROM json_each(COALESCE(l.email_domains_json,'[]')) r
     WHERE d.value=json_extract(r.value,'$.exact') OR
       (length(d.value)>length(json_extract(r.value,'$.subdomains'))+1
        AND substr(d.value,-length(json_extract(r.value,'$.subdomains'))-1)='.'||json_extract(r.value,'$.subdomains')))))) ELSE 0 END
BEGIN SELECT RAISE(ABORT,'projection_invariant: unmatched domain evidence'); END;

CREATE TRIGGER link_domain_rules_replace BEFORE INSERT ON invitation_links
WHEN EXISTS(SELECT 1 FROM invitation_links old WHERE old.id=NEW.id
 AND COALESCE(old.email_domains_json,'[]') IS NOT COALESCE(NEW.email_domains_json,'[]'))
BEGIN SELECT RAISE(ABORT,'projection_invariant: immutable domain rules'); END;

CREATE TRIGGER request_domain_evidence_replace BEFORE INSERT ON invitation_requests
WHEN EXISTS(SELECT 1 FROM invitation_requests old WHERE old.id=NEW.id
 AND old.domain_evidence_json IS NOT NEW.domain_evidence_json)
BEGIN SELECT RAISE(ABORT,'projection_invariant: immutable domain evidence'); END;
