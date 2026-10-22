-- A synthetic Messages database for latch's host and CLI tests: two direct
-- chats and a group, a code that lives only in attributedBody (as most new
-- messages do), and a tapback. No real message; the order notice is the
-- captured business notification from messages-reader's fixtures.
create table handle (ROWID integer primary key, id text, service text);
create table chat (ROWID integer primary key, guid text, chat_identifier text,
                   display_name text, style integer, service_name text);
create table message (ROWID integer primary key, guid text, text text, attributedBody blob,
                      handle_id integer, date integer, is_from_me integer default 0,
                      associated_message_type integer default 0, item_type integer default 0,
                      service text);
create table chat_message_join (chat_id integer, message_id integer, message_date integer);
create table chat_handle_join (chat_id integer, handle_id integer);
create table message_attachment_join (message_id integer, attachment_id integer);
insert into handle values (10, '+15550000001', 'iMessage'), (20, 'bo@example.com', 'iMessage');
insert into chat values (1, 'iMessage;-;+15550000001', '+15550000001', NULL, 45, 'iMessage'),
                        (2, 'iMessage;-;bo@example.com', 'bo@example.com', NULL, 45, 'iMessage'),
                        (3, 'iMessage;+;chat333', 'chat333', 'Dinner', 43, 'iMessage');
insert into chat_handle_join values (1, 10), (2, 20), (3, 10), (3, 20);
insert into message (ROWID, text, attributedBody, handle_id, date, is_from_me, associated_message_type, service) values
  (101, 'lunch?', NULL, 10, (strftime('%s','now') - 978307200 - 7200) * 1000000000, 0, 0, 'iMessage'),
  (102, 'sure', NULL, NULL, (strftime('%s','now') - 978307200 - 7000) * 1000000000, 1, 0, 'iMessage'),
  (103, NULL, X'040B73747265616D747970656481E80384012B13596F757220636F64652069732034383239313386', 10, (strftime('%s','now') - 978307200 - 600) * 1000000000, 0, 0, 'iMessage'),
  (104, NULL, NULL, NULL, (strftime('%s','now') - 978307200 - 500) * 1000000000, 1, 2000, 'iMessage'),
  (201, NULL, X'040B73747265616D747970656481E803840140848484194E534D757461626C6541747472696275746564537472696E67008484124E5341747472696275746564537472696E67008484084E534F626A6563740085928484840F4E534D757461626C65537472696E67018484084E53537472696E67019584012B4A596F7572206F72646572207761732064656C69766572656421205468616E6B20796F7520666F72206F72646572696E672066726F6D20537570657220447570657220427572676572732E8684026949014A928484840C4E5344696374696F6E617279009584016901928498981D5F5F6B494D4D657373616765506172744174747269627574654E616D658692848484084E534E756D626572008484074E5356616C7565009584012A849B9B00868686', 20, (strftime('%s','now') - 978307200 - 3000) * 1000000000, 0, 0, 'iMessage'),
  (301, NULL, X'040B73747265616D747970656481E80384012B0B7461626C6520666F72203686', 10, (strftime('%s','now') - 978307200 - 4000) * 1000000000, 0, 0, 'iMessage');
insert into chat_message_join (chat_id, message_id) values (1, 101), (1, 102), (1, 103), (1, 104), (2, 201), (3, 301);
