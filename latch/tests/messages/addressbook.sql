-- A synthetic address book (the v22 store's tables, only the columns read).
create table ZABCDRECORD (Z_PK integer primary key, ZNICKNAME text, ZFIRSTNAME text, ZLASTNAME text, ZORGANIZATION text);
create table ZABCDPHONENUMBER (Z_PK integer primary key, ZOWNER integer, ZFULLNUMBER text);
create table ZABCDEMAILADDRESS (Z_PK integer primary key, ZOWNER integer, ZADDRESS text);
insert into ZABCDRECORD values (1, NULL, 'Ann', 'Example', NULL), (2, 'Bo', 'Robert', 'Sample', 'Sample & Co');
insert into ZABCDPHONENUMBER (ZOWNER, ZFULLNUMBER) values (1, '(555) 000-0001');
insert into ZABCDEMAILADDRESS (ZOWNER, ZADDRESS) values (2, 'bo@example.com');
