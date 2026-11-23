-- Raw AAC files (.aac, ADTS) are audio/aac, not audio/mp4 (that's .m4a and .mp4, AAC inside an MP4 box): apps that
-- go by the type (Chromecast, browsers) didn't play them.
UPDATE track SET content_type = 'audio/aac' WHERE suffix = 'aac' AND content_type = 'audio/mp4';
