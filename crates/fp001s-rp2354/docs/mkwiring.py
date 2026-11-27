# Generates wiring SVGs (Pico 2 <-> FP008 / FP001S). Pico 2 pinout: USB at top, left column pins 1..20 top->bottom,
# right column pins 40..21 top->bottom (Raspberry Pi Pico 2 datasheet).
import sys
FONT = "'Noto Sans JP','Noto Sans CJK JP','Hiragino Sans','Yu Gothic',Meiryo,sans-serif"
LEFT = {1:'GP0',2:'GP1',3:'GND',4:'GP2',5:'GP3',6:'GP4',7:'GP5',8:'GND'}
RIGHT = {40:'VBUS',39:'VSYS',38:'GND',37:'3V3_EN',36:'3V3(OUT)'}

def pico(x, y, used):
    s = []
    w, pitch, rows = 150, 16, 20
    h = rows * pitch + 30
    s.append(f'<rect x="{x}" y="{y}" width="{w}" height="{h}" rx="8" fill="#1b7a3a"/>')
    s.append(f'<rect x="{x+w/2-22}" y="{y-12}" width="44" height="22" rx="4" fill="#bbb"/>')
    s.append(f'<text x="{x+w/2}" y="{y+4}" font-size="11" text-anchor="middle">USB</text>')
    s.append(f'<text x="{x+w/2}" y="{y+h/2}" font-size="16" fill="#fff" text-anchor="middle" font-weight="bold">Pico 2</text>')
    s.append(f'<text x="{x+w/2}" y="{y+h/2+18}" font-size="11" fill="#cfe" text-anchor="middle">上から見た図</text>')
    pos = {}
    for i in range(rows):
        ly = y + 26 + i * pitch
        for n, px in ((i + 1, x + 12), (40 - i, x + w - 12)):
            on = n in used
            s.append(f'<circle cx="{px}" cy="{ly}" r="{6 if on else 4}" fill="{"#f5c518" if on else "#8fb"}" stroke="{"#7a5c00" if on else "none"}"/>')
            if on:
                tx = px + 10 if n <= 20 else px - 10
                anchor = 'start' if n <= 20 else 'end'
                s.append(f'<text x="{tx}" y="{ly+4}" font-size="11" fill="#fff" text-anchor="{anchor}" font-weight="bold">{n}</text>')
            pos[n] = (px, ly)
    return '\n'.join(s)

def diagram(title, sub, conn_name, rows, notes, used, fname, board=None):
    # rows: (fp_pin, fp_label, color, pico_pin or text, pico_label, dashed)
    W = 980; top = 110; rh = 44
    H = max(top + len(rows) * rh + 40 + len(notes) * 22 + 20, 470)
    s = [f'<svg xmlns="http://www.w3.org/2000/svg" width="{W}" height="{H}" viewBox="0 0 {W} {H}" font-family="{FONT}">',
         f'<rect width="{W}" height="{H}" fill="#fff"/>',
         f'<text x="24" y="40" font-size="24" font-weight="bold">{title}</text>',
         f'<text x="24" y="68" font-size="14" fill="#444">{sub}</text>',
         pico(24, top, used) if board is None else board(24, top)]
    xa, xb = 230, 620
    s.append(f'<text x="{xa}" y="{top-14}" font-size="14" font-weight="bold">{conn_name}</text>')
    s.append(f'<text x="{xb+20}" y="{top-14}" font-size="14" font-weight="bold">つなぐ先</text>')
    for i, (p, lab, col, dst, dlab, dashed) in enumerate(rows):
        y = top + 16 + i * rh
        s.append(f'<rect x="{xa}" y="{y-16}" width="170" height="32" rx="6" fill="#222"/>')
        s.append(f'<text x="{xa+12}" y="{y+5}" font-size="15" fill="#fff">{p}  {lab}</text>')
        da = ' stroke-dasharray="8 6"' if dashed else ''
        s.append(f'<line x1="{xa+170}" y1="{y}" x2="{xb}" y2="{y}" stroke="{col}" stroke-width="6" stroke-linecap="round"{da}/>')
        s.append(f'<circle cx="{xb+8}" cy="{y}" r="7" fill="#f5c518" stroke="#7a5c00"/>' if isinstance(dst, int) else f'<circle cx="{xb+8}" cy="{y}" r="7" fill="#ddd" stroke="#888"/>')
        txt = (f'Pico {dst} 番  {dlab}' if isinstance(dst, int) else dst) 
        s.append(f'<text x="{xb+22}" y="{y+5}" font-size="15">{txt}</text>')
    ny = top + len(rows) * rh + 34
    for k, n in enumerate(notes):
        s.append(f'<text x="{xa}" y="{ny + k*22}" font-size="14" fill="#333">{n}</text>')
    s.append('</svg>')
    open(fname, 'w').write('\n'.join(s))

out = sys.argv[1]
diagram('FP008 ⇔ Pico 2', 'FP008 の番号は、モジュールの基板のシルク「1」から数えた 8 ピンの番号。線の色は手元のケーブルで確かめる。',
        'FP008(8 ピン)', [
    (1, '5V', '#d32f2f', 40, 'VBUS (5V)', False),
    (2, 'GND', '#222', 38, 'GND', False),
    (3, 'TXD(出力)', '#43a047', 2, 'GP1 = UART0 RX', False),
    (4, 'RXD(入力)', '#fbc02d', 1, 'GP0 = UART0 TX', False),
    (5, 'IRQ(人感)', '#8e24aa', 4, 'GP2', False),
    (6, '3.3V', '#fb8c00', 36, '3V3(OUT)', False),
    (7, 'D+', '#9e9e9e', 'USB ケーブルの緑 → PC(カメラ)', '', True),
    (8, 'D−', '#9e9e9e', 'USB ケーブルの白 → PC(カメラ)', '', True),
 ], ['・3 TXD → GP1、4 RXD ← GP0: 送りと受けを交差させる', '・USB ケーブルの黒は GND へ、赤(5V)はつながない',
     '・試すときだけ: 表側の赤外線の設定パッド 2 TX → Pico 7 番 GP5、4 GND → Pico 8 番 GND'],
 {1,2,4,7,8,36,38,40}, f'{out}/wiring-fp008.svg')

diagram('FP001S ⇔ Pico 2', '色は FP001S の付属ケーブル(コネクタの端から)。GND は緑。3.3V は AMS1117-3.3 から(Pico の 3V3 では足りない)。',
        'FP001S(6 ピン)', [
    (1, '赤 3.3V', '#d32f2f', 'AMS1117-3.3 の出力(入力は USB-C 変換基板の 5V)', '', False),
    (2, '黒 TXD(出力)', '#222', 2, 'GP1 = UART0 RX', False),
    (3, '黄 RXD(入力)', '#fbc02d', 1, 'GP0 = UART0 TX', False),
    (4, '緑 GND', '#43a047', 38, 'GND(AMS1117 の GND とも共通)', False),
    (5, '青 D−', '#1e88e5', 'USB-C 変換基板の D−(カメラ)', '', True),
    (6, '白 D+', '#9e9e9e', 'USB-C 変換基板の D+(カメラ)', '', True),
 ], ['・USB-C 変換基板は A 口の充電器 + A→C から(PC の USB だと顔の処理で再起動)',
     '・GND は FP001S・AMS1117・Pico 2 を全部共通に(忘れると UART が 0V)',
     '・人感センサー(IRQ)は FP001S には無い'],
 {1,2,38}, f'{out}/wiring-fp001s.svg')

def pl2303(x, y):
    return '\n'.join([f'<rect x="{x}" y="{y}" width="150" height="150" rx="8" fill="#37474f"/>',
                      f'<text x="{x+75}" y="{y+60}" font-size="16" fill="#fff" text-anchor="middle" font-weight="bold">PL2303</text>',
                      f'<text x="{x+75}" y="{y+82}" font-size="11" fill="#cfd8dc" text-anchor="middle">USB-TTL 3.3V</text>',
                      f'<text x="{x+75}" y="{y+100}" font-size="11" fill="#cfd8dc" text-anchor="middle">→ PC(/dev/ttyUSB0)</text>'])

diagram('FP008 と FP001S を同時に', 'FP008 は上の図のとおり Pico 2 へ。FP001S の UART は PL2303 の USB-TTL ケーブル(3.3V)で PC へ直接(ファームは変えない)。',
        'FP001S(6 ピン)', [
    (1, '赤 3.3V', '#d32f2f', 'AMS1117-3.3 の出力(PL2303 の赤 VCC はつながない)', '', False),
    (2, '黒 TXD(出力)', '#222', 'PL2303 の RXD(白のことが多い)', '', False),
    (3, '黄 RXD(入力)', '#fbc02d', 'PL2303 の TXD(緑のことが多い)', '', False),
    (4, '緑 GND', '#43a047', 'PL2303 の GND(黒)と AMS1117 の GND', '', False),
    (5, '青 D−', '#1e88e5', 'USB-C 変換基板の D−(カメラ)', '', True),
    (6, '白 D+', '#9e9e9e', 'USB-C 変換基板の D+(カメラ)', '', True),
 ], ['・PL2303 の線の色は製品のラベルで確かめる。115200 8N1',
     '・FP001S は 400 mA 食うので PL2303 からは給電しない',
     '・Pico 2 側(FP008)はそのまま: /dev/ttyACM1 が FP008、/dev/ttyUSB0 が FP001S'],
 set(), f'{out}/wiring-dual.svg', board=pl2303)
