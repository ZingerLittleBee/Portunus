#!/usr/bin/env python3
"""
检查 PR 是否有满足：非节假日 且 在 10:00-19:00 之间的
"""
import json
from datetime import datetime, timezone, timedelta

# 2026年中国法定节假日
HOLIDAYS_2026 = {
    (1, 1), (1, 2), (1, 3),
    (2, 17), (2, 18), (2, 19), (2, 20), (2, 21), (2, 22), (2, 23),
    (4, 4), (4, 5), (4, 6),
    (5, 1), (5, 2), (5, 3), (5, 4), (5, 5),
    (6, 19), (6, 20), (6, 21),
    (9, 25), (9, 26), (9, 27),
    (10, 1), (10, 2), (10, 3), (10, 4), (10, 5), (10, 6), (10, 7),
}

def is_weekend(dt):
    return dt.weekday() >= 5

def is_holiday(dt):
    if is_weekend(dt):
        return True
    return (dt.month, dt.day) in HOLIDAYS_2026

def is_within_work_hours(dt):
    hour = dt.hour
    minute = dt.minute
    time_val = hour * 60 + minute
    start = 10 * 60
    end = 19 * 60
    return start <= time_val < end

# PR 数据（从 gh pr list 获取）
prs_json = '''[
  {"number":4,"title":"docs: forward-rs user documentation and Chinese i18n","createdAt":"2026-05-10T07:53:52Z","mergedAt":"2026-05-10T07:57:54Z","state":"MERGED"},
  {"number":3,"title":"feat(v0.11): rate limiting & QoS — per-rule + per-owner caps","createdAt":"2026-05-09T14:51:44Z","mergedAt":"2026-05-09T15:53:29Z","state":"MERGED"},
  {"number":2,"title":"v0.6.0 — operator Web UI (006-management-web-ui)","createdAt":"2026-05-07T17:35:29Z","mergedAt":"2026-05-07T18:00:39Z","state":"MERGED"},
  {"number":1,"title":"feat: port-range forwarding rules (002-port-range-forward, v0.2.0)","createdAt":"2026-05-07T01:22:00Z","mergedAt":"2026-05-07T01:46:31Z","state":"MERGED"}
]'''

prs = json.loads(prs_json)

print("=" * 80)
print("PR 时间分析（转换为北京时间 UTC+8）")
print("=" * 80)

found_any = False

for pr in prs:
    num = pr['number']
    title = pr['title']
    created_utc = datetime.fromisoformat(pr['createdAt'].replace('Z', '+00:00'))
    merged_utc = datetime.fromisoformat(pr['mergedAt'].replace('Z', '+00:00'))
    
    # 转换为北京时间 (+8)
    tz_utc8 = timezone(timedelta(hours=8))
    created_cn = created_utc.astimezone(tz_utc8)
    merged_cn = merged_utc.astimezone(tz_utc8)
    
    created_date = created_cn.date()
    merged_date = merged_cn.date()
    
    is_created_holiday = is_holiday(created_date)
    is_merged_holiday = is_holiday(merged_date)
    
    created_in_hours = is_within_work_hours(created_cn)
    merged_in_hours = is_within_work_hours(merged_cn)
    
    created_ok = not is_created_holiday and created_in_hours
    merged_ok = not is_merged_holiday and merged_in_hours
    
    print(f"\nPR #{num}: {title}")
    print(f"  创建时间: {created_cn.strftime('%Y-%m-%d %H:%M:%S %a')} (UTC: {pr['createdAt']})")
    print(f"    → 节假日? {'是' if is_created_holiday else '否'} | 10:00-19:00? {'是' if created_in_hours else '否'}")
    print(f"  合并时间: {merged_cn.strftime('%Y-%m-%d %H:%M:%S %a')} (UTC: {pr['mergedAt']})")
    print(f"    → 节假日? {'是' if is_merged_holiday else '否'} | 10:00-19:00? {'是' if merged_in_hours else '否'}")
    
    if created_ok or merged_ok:
        found_any = True
        print(f"  ✅ 满足条件！")
    else:
        print(f"  ❌ 不满足")

print("\n" + "=" * 80)
if found_any:
    print("结论：存在满足「非节假日 且 10:00-19:00」的 PR")
else:
    print("结论：没有 PR 满足「非节假日 且 10:00-19:00」")
    print("所有 PR 的创建/合并时间要么在节假日/周末，要么在工作日的非工作时段（凌晨或深夜）。")
