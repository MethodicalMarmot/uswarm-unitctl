import sys

for line in sys.stdin:
    if line.startswith("+QENG"):
        print(line)
        earfcn = rsrp = rsrq = rssi = rssnr = tx_power = None
        state, mode, is_tdd, mcc, mnc, cell_id, pcid, earfcn, freq_band_ind, \
            ul_bandwidth, dl_bandwidth, tac, rsrq, rsrp, rssi, rssnr, cqi, tx_power, srxlev = \
            line.split(':')[1].strip().split(',')
        rssi = int(rssi)
        rsrp = int(rsrp)
        rsrq = int(rsrq)
        rssnr = int(rssnr)

        if earfcn or rsrp or rsrq or rssi or rssnr:
            print(f"rssi: {rssi}, rsrp: {rsrp}, rsrq: {rsrq}, rssnr: {rssnr}, tx_power: {tx_power}")
