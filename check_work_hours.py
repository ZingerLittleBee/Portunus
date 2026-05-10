#!/usr/bin/env python3
"""
检查是否有 commit 同时满足：非节假日 且 在 10:00-19:00 之间
"""
import subprocess
from datetime import datetime

# 2026年中国法定节假日
HOLIDAYS_2026 = {
    (1, 1), (1, 2), (1, 3),  # 元旦
    (2, 17), (2, 18), (2, 19), (2, 20), (2, 21), (2, 22), (2, 23),  # 春节
    (4, 4), (4, 5), (4, 6),  # 清明节
    (5, 1), (5, 2), (5, 3), (5, 4), (5, 5),  # 劳动节
    (6, 19), (6, 20), (6, 21),  # 端午节
    (9, 25), (9, 26), (9, 27),  # 中秋节
    (10, 1), (10, 2), (10, 3), (10, 4), (10, 5), (10, 6), (10, 7),  # 国庆节
}

def is_weekend(dt):
    return dt.weekday() >= 5

def is_holiday(dt):
    if is_weekend(dt):
        return True
    return (dt.month, dt.day) in HOLIDAYS_2026

def is_within_work_hours(dt):
    """检查时间是否在 10:00 - 19:00 之间（含 10:00，不含 19:00）"""
    hour = dt.hour
    minute = dt.minute
    time_val = hour * 60 + minute
    start = 10 * 60  # 10:00
    end = 19 * 60    # 19:00
    return start <= time_val < end

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
        dt = datetime.strptime(dt_str.strip(), '%Y-%m-%d %H:%M:%S %z')
        commits.append({
            'sha': sha[:8],
            'dt': dt,
            'date_str': dt.strftime('%Y-%m-%d'),
            'time_str': dt.strftime('%H:%M:%S'),
            'weekday': dt.strftime('%A'),
            'msg': msg,
        })
    
    # 去重
    seen_sha = set()
    unique_commits = []
    for c in commits:
        if c['sha'] not in seen_sha:
            seen_sha.add(c['sha'])
            unique_commits.append(c)
    
    # 筛选：非节假日 且 10:00-19:00
    filtered = []
    for c in unique_commits:
        dt_local = c['dt'].replace(tzinfo=None)
        if not is_holiday(dt_local) and is_within_work_hours(dt_local):
            filtered.append(c)
    
    if not filtered:
        print("没有找到同时满足「非节假日」且「在 10:00-19:00 之间」的 commit。")
        print(f"\n总共分析了 {len(unique_commits)} 个 unique commit。")
        print("\n非节假日的 commit 时间分布：")
        
        non_holiday = [c for c in unique_commits if not is_holiday(c['dt'].replace(tzinfo=None))]
        for c in non_holiday:
            print(f"  {c['date_str']} ({c['weekday']}) {c['time_str']}  {c['sha']}  {c['msg'][:60]}")
    else:
        print(f"找到 {len(filtered)} 个满足条件的 commit（非节假日 + 10:00-19:00）：\n")
        for c in filtered:
            print(f"{c['sha']} | {c['date_str']} ({c['weekday']}) {c['time_str']} | {c['msg']}")

if __name__ == '__main__':
    main()
