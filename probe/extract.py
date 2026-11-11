# extract.py DOC OUTDIR: the step 3 blocks of docs/UPDATE-AGENT.md, byte for byte, as the probe's files.
import sys, re, hashlib
doc, out = sys.argv[1], sys.argv[2]
s = open(doc, newline='').read()
sec = s[s.index('## 3. Run the update'):s.index('## 4.')]
blocks = re.findall(r'```(bash|powershell)\n(.*?)```', sec, re.S)
kinds = [k for k, _ in blocks]
assert kinds == ['bash', 'powershell', 'powershell', 'bash'], kinds
names = ['step3.sh', 'sot-update.ps1', 'run-ps.txt', 'run-gitbash.txt']
for name, (_, body) in zip(names, blocks):
    open(f'{out}/{name}', 'w', newline='').write(body)
    print(name, hashlib.sha256(body.encode()).hexdigest()[:16])
