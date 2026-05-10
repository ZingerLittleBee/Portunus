#!/usr/bin/env python3
"""
分析 commit 时间，筛选非节假日且非 10:00-19:00 的 commit
"""
import subprocess
from datetime import datetime, timedelta
import re

# 2026年中国法定节假日（调休前）
HOLIDAYS_2026 = {
    # 元旦
    (1, 1), (1, 2), (1, 3),
    # 春节 (2月17日除夕, 2月23日初六)
    (2, 17), (2, 18), (2, 19), (2, 20), (2, 21), (2, 22), (2, 23),
    # 清明节
    (4, 4), (4, 5), (4, 6),
    # 劳动节 (5月1日-5日)
    (5, 1), (5, 2), (5, 3), (5, 4), (5, 5),
    # 端午节
    (6, 19), (6, 20), (6, 21),
    # 中秋节
    (9, 25), (9, 26), (9, 27),
    # 国庆节
    (10, 1), (10, 2), (10, 3), (10, 4), (10, 5), (10, 6), (10, 7),
}

def is_weekend(dt):
    return dt.weekday() >= 5  # 5=周六, 6=周日

def is_public_holiday(dt):
    return (dt.month, dt.day) in HOLIDAYS_2026

def is_holiday(dt, include_weekend=True):
    if include_weekend and is_weekend(dt):
        return True
    return is_public_holiday(dt)

def is_outside_work_hours(dt):
    """检查时间是否在 10:00 - 19:00 之外"""
    hour = dt.hour
    minute = dt.minute
    time_val = hour * 60 + minute
    start = 10 * 60  # 10:00
    end = 19 * 60    # 19:00
    return time_val < start or time_val >= end

def main():
    result = subprocess.run(
        ['git', 'log', '--pretty=format:%H|%ci|%s', '--all'],
        capture_output=True, text=True, cwd='/Users/zingerbee/Documents/forward-rs'
    )
    
    lines = result.stdout.strip().split('\n')
    
    commits = []
    for line in lines:
        parts = line.split('|', 2)
        if len(parts) != 3:
            continue
        sha, dt_str, msg = parts
        # dt_str 格式: 2026-05-10 21:21:52 +0800
        dt = datetime.strptime(dt_str.strip(), '%Y-%m-%d %H:%M:%S %z')
        commits.append({
            'sha': sha[:8],
            'dt': dt,
            'date_str': dt.strftime('%Y-%m-%d'),
            'time_str': dt.strftime('%H:%M:%S'),
            'weekday': dt.strftime('%A'),
            'msg': msg,
        })
    
    # 去重（有些 commit 被 merge 了两次）
    seen_sha = set()
    unique_commits = []
    for c in commits:
        if c['sha'] not in seen_sha:
            seen_sha.add(c['sha'])
            unique_commits.append(c)
    
    print(f"总共 {len(unique_commits)} 个 unique commit\n")
    
    # 筛选：非节假日 且 非 10:00-19:00
    filtered = []
    for c in unique_commits:
        dt_local = c['dt'].replace(tzinfo=None)
        if not is_holiday(dt_local, include_weekend=True) and is_outside_work_hours(dt_local):
            filtered.append(c)
    
    print(f"满足条件（非节假日 且 时间不在 10:00-19:00）的 commit: {len(filtered)} 个\n")
    print("-" * 80)
    
    for c in filtered:
        print(f"{c['sha']} | {c['date_str']} ({c['weekday']}) {c['time_str']} | {c['msg']}")
    
    print("\n" + "=" * 80)
    print("\n按日期统计：")
    date_stats = {}
    for c in filtered:
        d = c['date_str']
        if d not in date_stats:
            date_stats[d] = []
        date_stats[d].append(c)
    
    for d in sorted(date_stats.keys()):
        print(f"\n{d} ({date_stats[d][0]['weekday']}): {len(date_stats[d])} commits")
        for c in date_stats[d]:
            print(f"  {c['time_str']}  {c['sha']}  {c['msg'][:60]}")
    
    # 额外显示：所有非 10:00-19:00 的 commit（不管是否节假日）
    print("\n\n" + "=" * 80)
    print("\n所有非 10:00-19:00 的 commit（含节假日）:")
    outside_hours = [c for c in unique_commits if is_outside_work_hours(c['dt'].replace(tzinfo=None))]
    for c in outside_hours:
        is_h = is_holiday(c['dt'].replace(tzinfo=None), include_weekend=True)
        h_mark = "[节假日/周末]" if is_h else "[工作日]"
        print(f"{c['sha']} | {c['date_str']} ({c['weekday']}) {c['time_str']} {h_mark} | {c['msg'][:70]}")

if __name__ == '__main__':
    main()
