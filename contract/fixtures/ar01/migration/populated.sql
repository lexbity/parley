UPDATE agent_provider SET endpoint='http://localhost:11434',model='legacy:chat',embed_model='legacy:embed';
INSERT INTO entity(id,kind,name,revision,created_at,updated_at)
VALUES ('recipient','person','Recipient',1,'2026-11-01','2026-11-01'),
       ('resource','project','Resource',1,'2026-11-01','2026-11-01');
UPDATE entity SET subtype='gift',subtype_locked=1,body_json='{"title":"Resource"}',readiness='ready'
WHERE id='resource';
INSERT INTO opportunity(id,title,state,revision,created_at,updated_at)
VALUES ('opp','Opportunity','open',1,'2026-11-01','2026-11-01');
INSERT INTO opportunity_party(opportunity_id,party_id,role,added_at)
VALUES ('opp','recipient','primary','2026-11-01');
INSERT INTO source_content(id,normalized_sha256,normalization_version,normalized_text,created_at)
VALUES ('source','hash',1,'Saved source.','2026-11-01');
INSERT INTO source_variant(id,content_id,raw_sha256,raw_text,captured_at)
VALUES ('variant','source','hash','Saved source.','2026-11-01');
INSERT INTO source_occurrence(id,source_variant_id,actor_kind,captured_at)
VALUES ('occurrence','variant','owner','2026-11-01');

INSERT INTO durable_job(
 id,role,status,input_json,input_sha256,scope_kind,scope_id,current_attempt,created_at,updated_at,completed_at
)
VALUES ('interview','interviewer','succeeded','{"private":"AR01_CANARY_INPUT"}','hash','owner','chat',1,'2026-11-01','2026-11-01','2026-11-01'),
       ('brief','briefer','succeeded','{}','hash','party','recipient',1,'2026-11-01','2026-11-01','2026-11-01'),
       ('coach','coach','succeeded','{}','hash','owner','weekly',1,'2026-11-01','2026-11-01','2026-11-01'),
       ('composer','composer','succeeded','{}','hash','opportunity','opp',1,'2026-11-01','2026-11-01','2026-11-01'),
       ('linter','linter','succeeded','{}','hash','material','material',1,'2026-11-01','2026-11-01','2026-11-01'),
       ('matcher','matcher','succeeded','{}','hash','opportunity','opp',1,'2026-11-01','2026-11-01','2026-11-01'),
       ('pending-coach','coach','running','{}','hash','owner','weekly',1,'2026-11-01','2026-11-01',NULL);
INSERT INTO durable_job(
 id,role,status,input_json,input_sha256,source_occurrence_id,source_variant_id,
 scope_kind,scope_id,current_attempt,created_at,updated_at,completed_at
)
VALUES ('extract','extractor','succeeded','{}','hash','occurrence','variant',
        'source','occurrence',1,'2026-11-01','2026-11-01','2026-11-01');

INSERT INTO job_attempt (
 job_id,attempt_no,status,model,provider_snapshot_json,prompt_id,prompt_sha256,
 input_sha256,input_json,output_json,started_at,finished_at
)
VALUES ('interview',1,'succeeded','model','{}','prompt','hash','hash','{}','{"private":"AR01_CANARY_ATTEMPT"}','2026-11-01','2026-11-01'),
       ('pending-coach',1,'running','model','{}','prompt','hash','hash','{}',NULL,'2026-11-01',NULL);

INSERT INTO agent_chat(id,role,scope_kind,asset_kind,created_at,updated_at)
VALUES ('chat','interviewer','owner','gift','2026-11-01','2026-11-01');
INSERT INTO agent_turn(id,chat_id,turn_no,speaker,text,job_id,created_at)
VALUES ('turn','chat',1,'agent','Persisted application chat.','interview','2026-11-01');
INSERT INTO interviewer_candidate(id,chat_id,job_id,asset_kind,fields_json,owner_quotes_json,missing_fields_json,state,resource_id,created_at,updated_at)
VALUES ('candidate','chat','interview','gift','{}','{}','[]','saved','resource','2026-11-01','2026-11-01');

INSERT INTO brief_item(id,scope_kind,scope_id,kind,text,detail_json,tier,
 check_suggestion,speaker_kind,confirmed_by_owner,status,job_id,created_at,updated_at)
VALUES ('retained-brief','party','recipient','norm','Owner verified.','{}',3,NULL,'owner',1,'confirmed','brief','2026-11-01','2026-11-01'),
       ('pending-brief','party','recipient','norm','AR01_CANARY_BRIEF','{}',2,'Check with owner.','model_prior',0,'proposed','brief','2026-11-01','2026-11-01');
INSERT INTO brief_item(id,scope_kind,scope_id,kind,text,detail_json,tier,speaker_kind,confirmed_by_owner,status,supersedes_id,job_id,created_at,updated_at)
VALUES ('corrected-brief','party','recipient','norm','Owner corrected.','{}',3,'owner',1,'corrected','retained-brief','brief','2026-11-01','2026-11-01');

INSERT INTO coach_report(id,period,input_digest,event_cursor,job_id,report_json,created_at)
VALUES ('report','2026-W44','hash',1,'coach','{"summary":"AR01_CANARY_REPORT","evidence":{"candidate_count":2}}','2026-11-01');
INSERT INTO coach_question(id,job_id,question_key,kind,target_json,answer_schema_json,priority,
 impact,text,status,disposition_json,created_at,updated_at)
VALUES ('open-coach','coach','open','insight','{}','{}',0,1,'AR01_CANARY_QUESTION','open',NULL,'2026-11-01','2026-11-01'),
       ('resolved-coach','coach','resolved','insight','{}','{}',1,1,'AR01_CANARY_RESOLVED_TEXT','answered','{"note":"Owner answer."}','2026-11-01','2026-11-01');

INSERT INTO template_family(id,name,created_at) VALUES('family','Template','2026-11-01');
INSERT INTO template_version(id,family_id,version_no,body,fallbacks_json,status,content_sha256,created_at)
VALUES ('template','family',1,'Template body.','{}','active','hash','2026-11-01');
INSERT INTO material(id,opportunity_id,recipient_id,kind,head_version,created_at,updated_at)
VALUES ('material','opp','recipient','outreach',1,'2026-11-01','2026-11-01');
INSERT INTO material_version(material_id,version_no,opportunity_id,recipient_id,template_version_id,
 exchange_version,body,slot_fills_json,sentence_bindings_json,content_sha256,created_at,job_id)
VALUES ('material',1,'opp','recipient','template',1,'Unchanged approved delivery.','{}','[]',
 '69b4fdb0602607f24c6990bdb3055fe78e4a78645caf83590ea704c384306937','2026-11-01','composer');
INSERT INTO version_dependency(material_id,version_no,kind,source_id,source_revision)
VALUES ('material',1,'resource','resource','1');
INSERT INTO material_lint(material_id,version_no,content_sha256,deterministic_json,linter_job_id,created_at)
VALUES ('material',1,'69b4fdb0602607f24c6990bdb3055fe78e4a78645caf83590ea704c384306937','[]','linter','2026-11-01');
INSERT INTO resource_use(material_id,version_no,resource_id,slot_name)
VALUES ('material',1,'resource','gift');
INSERT INTO delivered_resource_version(resource_id,material_id,version_no,resource_revision,resource_title)
VALUES ('resource','material',1,1,'Resource');
INSERT INTO approval(id,material_id,version_no,content_sha256,dependency_vector_json,gate_snapshot_json,overrides_json,approved_at)
VALUES ('approval','material',1,'69b4fdb0602607f24c6990bdb3055fe78e4a78645caf83590ea704c384306937','[]','{}','[]','2026-11-01');
INSERT INTO release_record(id,approval_id,material_id,version_no,content_sha256,issued_at)
VALUES ('release','approval','material',1,'69b4fdb0602607f24c6990bdb3055fe78e4a78645caf83590ea704c384306937','2026-11-01');
INSERT INTO release_ack(release_id,succeeded,acknowledged_at) VALUES ('release',1,'2026-11-01');
INSERT INTO contact_thread(id,opportunity_id,recipient_id,state,current_touch,created_at,updated_at)
VALUES ('thread','opp','recipient','waiting',1,'2026-11-01','2026-11-01');
INSERT INTO send(id,thread_id,touch_no,primary_material_id,primary_version_no,recipient_id,gate_snapshot_json,
 overrides_json,content_sha256,sent_at,response_deadline,deadline_zone)
VALUES ('send','thread',1,'material',1,'recipient','{}','[]','69b4fdb0602607f24c6990bdb3055fe78e4a78645caf83590ea704c384306937',
 '2026-11-01','2026-11-30','UTC');
INSERT INTO send_part(send_id,material_id,version_no,release_id,kind)
VALUES ('send','material',1,'release','primary');

INSERT INTO match_run(id,opportunity_id,recipient_id,opportunity_revision,recipient_revision,
 need_sha256,embed_model_digest,calibration_json,shortlist_json,why_job_id,created_at)
VALUES ('match','opp','recipient',1,1,'hash','embed','{}','{"results":[]}','matcher','2026-11-01');
INSERT INTO proposal_batch(id,source_variant_id,job_id,status,created_at)
VALUES ('batch','variant','extract','pending','2026-11-01');
INSERT INTO proposal_item(id,batch_id,proposal_kind,candidate_json,occurrence_id,status)
VALUES ('staged','batch','claim','{"wording":"Saved source."}','occurrence','pending');
