-- Инициализация базы данных для SomnyToo
CREATE SCHEMA IF NOT EXISTS tvoiisonsecrets;

-- Привилегии для пользователя
GRANT ALL PRIVILEGES ON SCHEMA tvoiisonsecrets TO root;
GRANT ALL PRIVILEGES ON ALL TABLES IN SCHEMA tvoiisonsecrets TO root;
GRANT ALL PRIVILEGES ON ALL SEQUENCES IN SCHEMA tvoiisonsecrets TO root;
ALTER DEFAULT PRIVILEGES IN SCHEMA tvoiisonsecrets GRANT ALL ON TABLES TO root;