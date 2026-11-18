# SPDX-License-Identifier: MIT OR Apache-2.0
# SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

"""Curate executable search examples without using the translator as an oracle.

Run from any directory. Expectations below are authored from the public DSL
contract; this script only computes original UTF-8 byte spans and stable IDs.
The checked-in JSONL is the test input, so tests do not need Python.
"""

import json
import re
import sys
from pathlib import Path

PATH = Path(sys.argv[1]) if len(sys.argv)>1 else Path(__file__).with_name("corpus.jsonl")
old = [json.loads(line) for line in PATH.read_text(encoding="utf-8").splitlines()]
cases = [row for row in old if not row.get("id") or row["id"].startswith("base-")]
custom = [row for row in old if row.get("id") and not row["id"].startswith("base-")]
for index, row in enumerate(cases):
    row["id"] = f"base-{index + 1:03}"
    if row["input"] == "files not pdf":
        row.pop("diagnostic", None)
        row["dsl"] = "file and not (ext = pdf)"
        row["entities"] = [["Kind", "files", "file"], ["Exclusion", "not", "not"], ["FileType", "pdf", "pdf"]]
    if row["input"] == "files modified in the last 7 days":
        row["dsl"] = "file and modified >= @1794398400 and modified <= @1795003200"


def add(case_id, text, dsl, *entities):
    cases.append({"id": case_id, "input": text, "dsl": dsl, "entities": list(entities)})


def bad(case_id, text, diagnostic):
    cases.append({"id": case_id, "input": text, "diagnostic": diagnostic})


K = ["Kind", "files", "file"]
DK = ["Kind", "folders", "dir"]
for word in ["except", "excluding", "without", "other than", "but not"]:
    add("exclude-" + word.replace(" ", "-"), f"files {word} pdf", "file and not (ext = pdf)", K,
        ["Exclusion", word, "not"], ["FileType", "pdf", "pdf"])
add("exclude-list", "files except jpg and png", "file and not (ext in (jpg, png))", K,
    ["Exclusion", "except", "not"], ["FileType", "jpg", "jpg"], ["Connector", "and", "and"], ["FileType", "png", "png"])
add("exclude-or", "files except pdf or docx", "file and not (ext in (pdf, docx))", K,
    ["Exclusion", "except", "not"], ["FileType", "pdf", "pdf"], ["Connector", "or", "or"], ["FileType", "docx", "docx"])
add("exclude-explicit-list", "files not extensions pdf or docx", "file and not (ext in (pdf, docx))", K,
    ["Exclusion", "not", "not"], ["FileType", "extensions pdf", "pdf"], ["Connector", "or", "or"], ["FileType", "docx", "docx"])
for word, operator in [("named", "="), ("containing", "~")]:
    add("exclude-name-" + word, f'files not {word} "backup"', f'file and not (name {operator} "backup")', K,
        ["Exclusion", "not", "not"], ["Name", f'{word} "backup"', "backup"])
add("exclude-path", 'files not in "cache"', 'file and not (path ~ "cache")', K,
    ["Exclusion", "not", "not"], ["Path", 'in "cache"', "cache"])
add("name-does-not-contain", 'files whose name does not contain "backup"', 'file and not (name ~ "backup")', K,
    ["Name", 'name does not contain "backup"', "not backup"])

for prefix in ["extension", "extensions", "ext"]:
    add("extension-or-" + prefix, f"files with {prefix} md or txt", "file and ext in (md, txt)", K,
        ["FileType", prefix + " md", "md"], ["Connector", "or", "or"], ["FileType", "txt", "txt"])
add("extensions-and", "files extensions md and txt", "file and ext in (md, txt)", K,
    ["FileType", "extensions md", "md"], ["Connector", "and", "and"], ["FileType", "txt", "txt"])
add("extensions-comma", "files extensions .md,.txt,.rst", "file and ext in (md, txt, rst)", K,
    ["FileType", "extensions .md", "md"], ["Connector", ",", "or"], ["FileType", ".txt", "txt"], ["Connector", ",", "or"], ["FileType", ".rst", "rst"])
add("generic-extension", "files extension parquet", "file and ext = parquet", K, ["FileType", "extension parquet", "parquet"])
add("generic-suffix", "files ending with .toml", "file and ext = toml", K, ["FileType", "ending with .toml", "toml"])
add("mixed-unit-range", "files between 1 MB and 2 GB", "file and size >= 1mib and size <= 2gib", K, ["Size", "between 1 MB and 2 GB", "1mib..2gib"])
add("equal-mixed-unit-range", "files between 512 MB and 0.5 GB", "file and size >= 512mib and size <= 0.5gib", K, ["Size", "between 512 MB and 0.5 GB", "512mib..0.5gib"])
add("type-or", "files pdf or xlsx", "file and ext in (pdf, xlsx)", K,
    ["FileType", "pdf", "pdf"], ["Connector", "or", "or"], ["FileType", "xlsx", "xlsx"])
add("type-or-before-subject", "pdf or xlsx files", "file and ext in (pdf, xlsx)",
    ["FileType", "pdf", "pdf"], ["Connector", "or", "or"], ["FileType", "xlsx", "xlsx"], K)
add("explicit-category-and", "files types pdf and xlsx", "file and ext in (pdf, xlsx)", K,
    ["FileType", "types pdf", "pdf"], ["Connector", "and", "and"], ["FileType", "xlsx", "xlsx"])
add("kind-or", "files or folders over 1 MB", "(file or dir) and size > 1mib", K,
    ["Connector", "or", "or"], DK, ["Size", "over 1 MB", "> 1mib"])

for word, flag in [("sparse", "sparse"), ("unreadable", "unreadable"), ("inaccessible", "unreadable"),
                   ("partial", "partial"), ("hardlinked", "hardlink")]:
    add("flag-" + word, f"{word} files", f"file and {flag}", ["Flag", word, flag], K)
add("empty-files", "empty files", "file and apparent = 0b", ["Flag", "empty", "apparent = 0b"], K)
add("files-empty", "files that are empty", "file and apparent = 0b", K, ["Flag", "empty", "apparent = 0b"])
add("empty-folders", "empty folders", "dir and empty", ["Flag", "empty", "empty"], DK)
add("empty-images", "empty images", "file and ext in (jpg, jpeg, png, gif, webp, bmp, tif, tiff, svg, heic, avif, ico) and apparent = 0b", ["Flag", "empty", "apparent = 0b"], ["FileType", "images", "jpg,jpeg,png,gif,webp,bmp,tif,tiff,svg,heic,avif,ico"])
add("documents-empty", "documents empty", "file and ext in (pdf, doc, docx, docm, odt, rtf, txt, md) and apparent = 0b", ["FileType", "documents", "pdf,doc,docx,docm,odt,rtf,txt,md"], ["Flag", "empty", "apparent = 0b"])
add("not-empty-folders", "folders not empty", "dir and not (empty)", DK, ["Exclusion", "not", "not"], ["Flag", "empty", "empty"])
add("sparse-and-size", "sparse files greater than 5 MB", "file and sparse and size > 5mib",
    ["Flag", "sparse", "sparse"], K, ["Size", "greater than 5 MB", "> 5mib"])
add("folder-items", "folders with more than 100 files", "dir and items > 100", DK, ["Count", "more than 100 files", "items > 100"])
add("folder-children", "folders at least 20 children", "dir and children >= 20", DK, ["Count", "at least 20 children", "children >= 20"])
for phrase, seconds, op in [("older than 30 days", 2592000, ">"), ("newer than 2 weeks", 1209600, "<"),
                             ("younger than 5 hours", 18000, "<"), ("age at least 90 minutes", 5400, ">="),
                             ("age at most 7d", 604800, "<=")]:
    add("age-" + phrase.replace(" ", "-"), "files " + phrase, f"file and age {op} {seconds}s", K, ["Age", phrase, f"{op} {seconds}s"])

for field in ["created", "modified"]:
    kind = field.title()
    for phrase, seconds in [("in last 24 hours", 86400), ("in the past 7 days", 604800),
                             ("past 90 minutes", 5400), ("last 2 weeks", 1209600), ("last 30min", 1800)]:
        add(field + "-" + phrase.replace(" ", "-"), "files " + field + " " + phrase,
            f"file and {field} >= @{1795003200-seconds} and {field} <= @1795003200", K,
            [kind, field + " " + phrase, f"last {seconds}s"])
    add(field + "-negated-interval", f"files not {field} in last 24 hours",
        f"file and not ({field} >= @1794916800 and {field} <= @1795003200)", K,
        ["Exclusion", "not", "not"], [kind, field + " in last 24 hours", "last 86400s"])
    for phrase, start, end in [("this week", "2026-11-16", "2026-11-23"),
                               ("this month", "2026-11-01", "2026-12-01"),
                               ("this year", "2026-01-01", "2027-01-01"),
                               ("last year", "2025-01-01", "2026-01-01")]:
        add(field + "-" + phrase.replace(" ", "-"), "files " + field + " " + phrase,
            f"file and {field} >= {start} and {field} < {end}", K, [kind, field + " " + phrase, f"{start}..{end}"])
    add(field + "-range", f"files {field} between 2026-11-01 and 2026-11-05",
        f"file and {field} >= 2026-11-01 and {field} < 2026-11-06", K,
        [kind, field + " between 2026-11-01 and 2026-11-05", "2026-11-01..2026-11-06"])
    add(field + "-on-or-before", f"files {field} on or before 2026-11-05", f"file and {field} < 2026-11-06", K,
        [kind, field + " on or before 2026-11-05", "< 2026-11-06"])
    add(field + "-on-or-after", f"files {field} on or after 2026-11-05", f"file and {field} >= 2026-11-05", K,
        [kind, field + " on or after 2026-11-05", ">= 2026-11-05"])

for phrase, op, value, kind in [
    ('starting with "report"', "startswith", "report", "Name"),
    ('beginning with "invoice"', "startswith", "invoice", "Name"),
    ('names start with "report"', "startswith", "report", "Name"),
    ('names ending with ".tar.gz"', "endswith", ".tar.gz", "Name"),
    ('ending with ".txt"', "endswith", ".txt", "Name"),
    ('ending with tar.gz', "endswith", "tar.gz", "Name"),
    ('path starts with "C:/Users"', "startswith", "C:/Users", "Path"),
    ('path ends with "cache"', "endswith", "cache", "Path"),
    ('name contains "budget"', "~", "budget", "Name"),
    ('named "or"', "=", "or", "Name"),
    ('named "pdf or txt"', "=", "pdf or txt", "Name"),
    ('containing "not pdf"', "~", "not pdf", "Name"),
    ('named "résumé"', "=", "résumé", "Name"),
]:
    add("text-" + re.sub(r"\W+", "-", phrase.lower()).strip("-"), "files " + phrase, f'file and {"path" if kind=="Path" else "name"} {op} "{value}"', K, [kind, phrase, value])
add("named-and-date", 'files named "report" modified yesterday', 'file and name = "report" and modified >= 2026-11-17 and modified < 2026-11-18', K,
    ["Name", 'named "report"', "report"], ["Modified", "modified yesterday", "2026-11-17..2026-11-18"])

for word, values in [
    ("typescript", "ts,tsx,mts,cts"), ("java", "java"), ("go", "go"), ("c#", "cs"),
    ("shell", "sh,bash,zsh,fish"), ("powershell", "ps1,psm1,psd1"), ("sql", "sql"),
    ("config", "toml,yaml,yml,ini,cfg,conf,json,xml"),
    ("presentations", "ppt,pptx,pptm,pps,ppsx,odp,key"), ("ebooks", "epub,mobi,azw,azw3,pdf"),
    ("fonts", "ttf,otf,woff,woff2"), ("database", "db,sqlite,sqlite3,mdb,accdb,sql"), ("logs", "log"),
]:
    exts = values.split(",")
    dsl = "ext = " + exts[0] if len(exts) == 1 else "ext in (" + ", ".join(exts) + ")"
    add("family-" + word.replace("#", "sharp"), word + " files", "file and " + dsl, ["FileType", word, values], K)

for index, (text, diagnostic) in enumerate([
    ("files not", "Incomplete"), ("files except", "Incomplete"), ("files pdf or", "Ambiguous"),
    ("files pdf and or txt", "connectors"), ("files extension and", "invalid"),
    ("files under 10", "interpret"), ("files whose contents contain invoice", "interpret"),
    ("folders without files", "ambiguous"), ("images and documents", "Ambiguous"),
    ("images or videos over 10 MB", "trailing"), ("files not pdf or png", "Ambiguous"),
    ("files named report final", "interpret"), ("files whose", "Incomplete"),
    ("files and please", "Incomplete"), ("files and and pdf", "connectors"),
    ("files modified last 2 months", "date"), ("files age > 6m", "duration"),
    ("files created in last 999999999999999999 weeks", "date"),
    ("files modified between 2026-11-05 and 2026-11-01", "start date"),
    ("files over 10 MB or under 1 MB", "Ambiguous"),
    ('files python "source"', "interpret"), ('word "documents"', "interpret"),
    ('files "or" folders', "interpret"), ('files pdf "or" txt', "interpret"),
    ("files between 20 MB and 10 MB", "start size"), ("files between 1 GB and 500 MB", "start size"),
    ("files extension ..md", "invalid"), ("files ..md", "interpret"),
    ('files created "last" week', "date"), ('files modified last "week"', "date"),
]):
    bad(f"adversarial-{index + 1:03}", text, diagnostic)

for slug, phrase, normalized in [
    ("ten-megabytes", "ten megabytes", "10mib"),
    ("hundred-kilobytes", "one hundred kilobytes", "100kib"),
    ("hundred-five-mb", "one hundred and five MB", "105mib"),
    ("thousand-bytes", "one thousand bytes", "1000b"),
    ("million-bytes", "a million bytes", "1000000b"),
    ("thousand-five-bytes", "one thousand and five bytes", "1005b"),
    ("decimal-gb", "one point five GB", "1.5gib"),
    ("decimal-digit-chain", "one point five zero GB", "1.5gib"),
    ("half-gb", "half a GB", "0.5gib"),
    ("a-half-gb", "a half GB", "0.5gib"),
    ("quarter-gb", "a quarter GB", "0.25gib"),
    ("mixed-fraction-gb", "one and a half GB", "1.5gib"),
    ("hyphenated-number", "twenty-four megabytes", "24mib"),
    ("zero-bytes", "zero bytes", "0b"),
    ("a-megabyte", "a megabyte", "1mib"),
]:
    add("quantity-size-"+slug,"files larger than "+phrase,"file and size > "+normalized,K,["Size","larger than "+phrase,"> "+normalized])
for slug, phrase, amount in [
    ("hundred","one hundred",100),("hundred-five","one hundred and five",105),
    ("thousand-five","one thousand and five",1005),("article-hundred","a hundred",100),
    ("hyphenated","twenty-four",24),("zero","zero",0),("exact-large-digits","9007199254740993",9007199254740993),
    ("million","two million one hundred and five",2000105),
]:
    add("quantity-count-"+slug,"folders with at least "+phrase+" files",f"dir and items >= {amount}",DK,["Count","at least "+phrase+" files",f"items >= {amount}"])
add("quantity-count-and-next-clause","folders at least one hundred files and modified yesterday","dir and items >= 100 and modified >= 2026-11-17 and modified < 2026-11-18",DK,["Count","at least one hundred files","items >= 100"],["Modified","modified yesterday","2026-11-17..2026-11-18"])
add("quantity-range-inherited-unit","files between one and two GB","file and size >= 1gib and size <= 2gib",K,["Size","between one and two GB","1gib..2gib"])
add("quantity-range-conjunction","files between one hundred and five and two hundred MB","file and size >= 105mib and size <= 200mib",K,["Size","between one hundred and five and two hundred MB","105mib..200mib"])
add("quantity-range-hundred-groups","files between one hundred and two hundred MB","file and size >= 100mib and size <= 200mib",K,["Size","between one hundred and two hundred MB","100mib..200mib"])
for amount in [9007199254740993,18446744073709551614,18446744073709551615]:
    add("quantity-exact-large-bytes-"+str(amount),f"files exactly {amount} bytes",f"file and size = {amount}b",K,["Size",f"exactly {amount} bytes",f"= {amount}b"])
for slug, phrase, seconds in [("two-days","two days",172800),("twenty-four-hours","twenty-four hours",86400),("half-hour","half an hour",1800),("half-day","half a day",43200),("one-point-five-hours","one point five hours",5400)]:
    # Half an hour is intentionally not in the strict article grammar yet.
    if slug=="half-hour": continue
    add("quantity-duration-"+slug,"files created in last "+phrase,f"file and created >= @{1795003200-seconds} and created <= @1795003200",K,["Created","created in last "+phrase,f"last {seconds}s"])
add("quantity-age","files older than twenty-four hours","file and age > 86400s",K,["Age","older than twenty-four hours","> 86400s"])
add("quantity-zero-duration","files created in last zero seconds","file and created >= @1795003200 and created <= @1795003200",K,["Created","created in last zero seconds","last 0s"])
bad("quantity-fractional-second","files created in last zero point five seconds","date")
bad("quantity-fractional-byte","files exactly zero point five bytes","interpret")
bad("quantity-fractional-count","folders at least zero point five files","interpret")

for field in ["created","modified"]:
    entity=field.title()
    for slug, phrase, start, end in [
        ("month-first","on November 5, 2026","2026-11-05","2026-11-06"),
        ("day-first","on 5 November 2026","2026-11-05","2026-11-06"),
        ("month-abbreviation","on Nov 5th 2026","2026-11-05","2026-11-06"),
        ("day-ordinal","on 5th November 2026","2026-11-05","2026-11-06"),
        ("leap-day","on February 29, 2024","2024-02-29","2024-03-01"),
        ("named-month","in October 2026","2026-10-01","2026-11-01"),
        ("december-rollover","in December 2025","2025-12-01","2026-01-01"),
        ("whole-year","in 2025","2025-01-01","2026-01-01"),
        ("two-days-ago","two days ago","2026-11-16","2026-11-17"),
        ("one-day-ago","one day ago","2026-11-17","2026-11-18"),
        ("zero-days-ago","zero days ago","2026-11-18","2026-11-19"),
        ("two-weeks-ago","two weeks ago","2026-11-04","2026-11-05"),
        ("last-tuesday","last Tuesday","2026-11-17","2026-11-18"),
        ("strict-last-wednesday","last Wednesday","2026-11-11","2026-11-12"),
    ]:
        add("calendar-"+field+"-"+slug,"files "+field+" "+phrase,f"file and {field} >= {start} and {field} < {end}",K,[entity,field+" "+phrase,start+".."+end])
    add("calendar-"+field+"-before-named",f"files {field} before November 5, 2026",f"file and {field} < 2026-11-05",K,[entity,field+" before November 5, 2026","< 2026-11-05"])
    add("calendar-"+field+"-named-range",f"files {field} between November 1, 2026 and November 5, 2026",f"file and {field} >= 2026-11-01 and {field} < 2026-11-06",K,[entity,field+" between November 1, 2026 and November 5, 2026","2026-11-01..2026-11-06"])
add("quantity-utf8-before-number",'files named "résumé" larger than ten megabytes','file and name = "résumé" and size > 10mib',K,["Name",'named "résumé"',"résumé"],["Size","larger than ten megabytes","> 10mib"])
add("quantity-name-literal",'files named "twenty-one.txt"','file and name = "twenty-one.txt"',K,["Name",'named "twenty-one.txt"',"twenty-one.txt"])

for index,text in enumerate([
    "files greater than one two MB","files greater than twenty thirteen MB","files greater than one hundred hundred MB",
    "files greater than one thousand million bytes","files greater than one hundred and MB","files greater than minus ten MB",
    "files greater than a few MB","files greater than one point point five GB","files greater than one point twenty GB",
    "files greater than one and two MB","files greater than two a GB","folders at least half files",
    "folders at least one point five files","files exactly half a byte","files exactly one point five bytes",
    "files between half a byte and two bytes","files larger than one billion TB",
]):
    bad(f"quantity-invalid-{index+1:03}",text,"interpret")
for index,phrase in enumerate([
    "on 03/04/2026","on 11/5/26","on November 31, 2026","on February 29, 2025","on November 5",
    "on Tuesday","Tuesday","next Tuesday","two months ago","two years ago","half days ago",
    "in December 9999","in 9999","on November 11st 2026","on 32 November 2026","in Octember 2026",
    "on 2026-11-05 at midnight","on November 5, 2026 UTC",
]):
    bad(f"calendar-invalid-{index+1:03}","files modified "+phrase,"date" if "at midnight" not in phrase and " UTC" not in phrase else "interpret")

for slug,qualifier,field in [
    ("apparent","apparent size","apparent"),("logical","logical size","apparent"),
    ("allocated","allocated size","size"),("hyphen-on-disk","on-disk size","size"),
    ("on-disk","on disk size","size"),("size-on-disk","size on disk","size"),
]:
    add("size-basis-"+slug,"files with "+qualifier+" greater than 10 MB",f"file and {field} > 10mib",K,["Size",qualifier+" greater than 10 MB",field+" > 10mib"])
    add("size-basis-range-"+slug,"files "+qualifier+" between one and two MB",f"file and {field} >= 1mib and {field} <= 2mib",K,["Size",qualifier+" between one and two MB",field+" 1mib..2mib"])
add("size-basis-spoken-apparent","files logical size at least one MB","file and apparent >= 1mib",K,["Size","logical size at least one MB","apparent >= 1mib"])
add("size-basis-allocated-under","files allocated size under 5 MB","file and size < 5mib",K,["Size","allocated size under 5 MB","size < 5mib"])
add("size-basis-independent-constraints","files apparent size over 1 MB and allocated size under 1 MB","file and apparent > 1mib and size < 1mib",K,["Size","apparent size over 1 MB","apparent > 1mib"],["Size","allocated size under 1 MB","size < 1mib"])
add("size-basis-not-sticky","files apparent size over 1 MB and size under 1 MB","file and apparent > 1mib and size < 1mib",K,["Size","apparent size over 1 MB","apparent > 1mib"],["Size","under 1 MB","< 1mib"])
add("size-basis-negation","files not apparent size greater than 10 MB","file and not (apparent > 10mib)",K,["Exclusion","not","not"],["Size","apparent size greater than 10 MB","apparent > 10mib"])
add("size-basis-negated-range","files not apparent size between 1 and 2 MB","file and not (apparent >= 1mib and apparent <= 2mib)",K,["Exclusion","not","not"],["Size","apparent size between 1 and 2 MB","apparent 1mib..2mib"])
add("size-basis-utf8-span",'files named "résumé" apparent size at least half a GB','file and name = "résumé" and apparent >= 0.5gib',K,["Name",'named "résumé"',"résumé"],["Size","apparent size at least half a GB","apparent >= 0.5gib"])
add("size-basis-legacy-size-prefix","files with size greater than 10 MB","file and size > 10mib",K,["Size","greater than 10 MB","> 10mib"])
add("size-basis-zero-allocation","files allocated size exactly zero bytes","file and size = 0b",K,["Size","allocated size exactly zero bytes","size = 0b"])
add("size-basis-zero-apparent","files apparent size exactly zero bytes","file and apparent = 0b",K,["Size","apparent size exactly zero bytes","apparent = 0b"])
add("size-basis-literal-name",'files named "apparent"','file and name = "apparent"',K,["Name",'named "apparent"',"apparent"])
add("size-basis-literal-path",'files in "on disk"','file and path ~ "on disk"',K,["Path",'in "on disk"',"on disk"])
add("size-basis-literal-substring",'files whose name contains "logical size"','file and name ~ "logical size"',K,["Name",'name contains "logical size"',"logical size"])
for index,text in enumerate([
    "files apparent size","files logical size","files allocated size under","files on disk",
    "files apparent allocated size over 1 MB","files apparent size allocated size over 1 MB",
    "files logical size between 1 MB and allocated 2 MB","folders allocated size more than ten files",
    "folders apparent size at least one hundred children","folders size on disk over 10 items",
    "files size on disk apparent size over 10 MB","files apparent greater than 10 MB",
    "files apparent size apparent size over 10 MB","files allocated size allocated size under 5 MB",
    "files apparent size greater than 10","files size on disk between 1 and 2",
]):
    bad(f"size-basis-invalid-{index+1:03}",text,"size")
bad("size-basis-unresolved-tail","files apparent size over 1 MB nonsense","interpret")
bad("size-basis-reversed","files logical size between 20 MB and 10 MB","start size")
bad("quantity-invalid-article-hundred","files over one hundred and a MB","interpret")
bad("quantity-invalid-article-thousand","files over one thousand and a MB","interpret")
add("quantity-valid-mixed-fraction-hundred","files over one hundred and a half MB","file and size > 100.5mib",K,["Size","over one hundred and a half MB","> 100.5mib"])

for field in ["created","modified"]:
    entity=field.title()
    for slug,comparison,age_op,timestamp_op in [
        ("older-strict","more than",">","<"),("older-inclusive","at least",">=","<="),
        ("newer-strict","less than","<",">"),("newer-inclusive","at most","<=",">="),
    ]:
        for duration_slug,phrase,seconds,subject in [("thirty-days","thirty days",2592000,"files"),("seven-days","7 days",604800,"files"),("two-weeks","two weeks",1209600,"folders")]:
            text=subject+" "+field+" "+comparison+" "+phrase+" ago"
            dsl=f'{"dir" if subject=="folders" else "file"} and {field} {timestamp_op} @{1795003200-seconds} and {field} <= @1795003200'
            add(f"timestamp-age-{field}-{slug}-{duration_slug}",text,dsl,DK if subject=="folders" else K,
                [entity,field+" "+comparison+" "+phrase+" ago",f"age {age_op} {seconds}s"])
    add("timestamp-age-negated-"+field,f"files not {field} less than seven days ago",f"file and not ({field} > @1794398400 and {field} <= @1795003200)",K,["Exclusion","not","not"],[entity,field+" less than seven days ago","age < 604800s"])
    add("timestamp-age-zero-"+field,f"files {field} at most zero seconds ago",f"file and {field} >= @1795003200 and {field} <= @1795003200",K,[entity,field+" at most zero seconds ago","age <= 0s"])
    add("timestamp-age-exact-epoch-"+field,f"files {field} at least 1795003200 seconds ago",f"file and {field} <= @0 and {field} <= @1795003200",K,[entity,field+" at least 1795003200 seconds ago","age >= 1795003200s"])
add("timestamp-age-fractional-minute","files created at most one point five minutes ago","file and created >= @1795003110 and created <= @1795003200",K,["Created","created at most one point five minutes ago","age <= 90s"])
add("timestamp-age-half-day","files modified less than half a day ago","file and modified > @1794960000 and modified <= @1795003200",K,["Modified","modified less than half a day ago","age < 43200s"])
add("timestamp-age-no-more-than","files modified no more than thirty days ago","file and modified >= @1792411200 and modified <= @1795003200",K,["Modified","modified no more than thirty days ago","age <= 2592000s"])
add("timestamp-age-no-less-than","folders created no less than two weeks ago","dir and created <= @1793793600 and created <= @1795003200",DK,["Created","created no less than two weeks ago","age >= 1209600s"])
add("timestamp-age-utf8-span",'files named "résumé" modified at most seven days ago','file and name = "résumé" and modified >= @1794398400 and modified <= @1795003200',K,["Name",'named "résumé"',"résumé"],["Modified","modified at most seven days ago","age <= 604800s"])
add("timestamp-age-upper-case","files modified MORE THAN thirty days AGO","file and modified < @1792411200 and modified <= @1795003200",K,["Modified","modified MORE THAN thirty days AGO","age > 2592000s"])
for index,(text,fragment) in enumerate([
    ("files modified more than thirty days","ago"),("files created less than 7 days before","ago"),
    ("files modified at least two weeks agone","ago"),("files created at most half a second ago","duration"),
    ("files modified more than 999999999999999999999 hours ago","duration"),
    ("files modified more than two months ago","duration"),("files created more than minutes ago","duration"),
    ("files created more than two billion seconds ago","epoch"),("files modified less than 7 days ago nonsense","interpret"),
    ('files modified less than seven days "ago"',"ago"),("files created exactly seven days ago","operator"),
    ("files modified less than one hundred and days ago","duration"),
    ("files created at most seven ago","duration"),("files created at least seven hours since","ago"),
]):
    bad(f"timestamp-age-invalid-{index+1:03}",text,fragment)

for case in cases:
    cursor = 0
    normalized = []
    for entity in case.get("entities", []):
        if isinstance(entity, dict):
            normalized.append(entity)
            cursor = entity["span"][1]
            continue
        kind, text, value = entity
        data = case["input"].encode("utf-8")
        excerpt = text.encode("utf-8")
        start = data.index(excerpt, cursor)
        end = start + len(excerpt)
        normalized.append({"kind": kind, "span": [start, end], "text": text, "value": value})
        cursor = end
    if "dsl" in case:
        case["entities"] = normalized
    else:
        case.pop("entities", None)
    assert len(case["input"].encode("utf-8")) < 16384
generated_ids={row["id"] for row in cases}
cases.extend(row for row in custom if row["id"] not in generated_ids)
assert len({row["id"] for row in cases}) == len(cases)
PATH.write_text("".join(json.dumps(row, ensure_ascii=False, separators=(",", ":")) + "\n" for row in cases), encoding="utf-8", newline="\n")
print(f"Wrote {len(cases)} curated examples")
