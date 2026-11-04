#!/bin/sh
# size of a git pack holding exactly the unique blobs of the sampled revisions (delta + zlib), for comparison with block compression
R=$HOME/rt/lab_data/repos
git --version
echo == ripgrep
cd $R/ripgrep
(for r in 9d1e619ff359b6e609b02f01e36952e603104bc6 919c5c72994edb378706594f6268542983eeee6d ebabe1df6a5922e88aad9024661aaf3a456200b1 a454fa75b9fafce01d41fd62787c15e54b07893d 79e5e6671f3f71e5c93d37bd3b53a8e8597ba1fc 4683a325fae596db75176d188034c6623e91db79 86c890bcecf885619565ce949ddb9099d55dd43c 17d09c0882babad2681dc99507127d8562696864 45473ba48f0561e970596ac979ba5d0bac92242a 968491f8e93e506b224ee4c85bee85f7ead0e5d5 fdde2bcd380488071c88d562fe6e1e5f1b39e47b 52d7f474206eb517c7284448777ed839c0cae8bb a16bfcb3d695d1d1158b20253806945581f80c80 9b88cf8b72fcc999fefa8186badd2d7084affd69 120e55e7c7502c843c8548f1d694ebdf72fc114c 2198bd92faacef6e4a102fb96f1640a17a7c3ed2 2ee690e87a59968eb67f3a5ede56bf99a895db02 edafb612d2b93b0dc28531abf155bdd7e3bcfe63 d47663b1b4548e4fa02d6e4b575718d0f5f5e7d6 91f5d22ef48bde459d90a8c7dbeab416e97d0f2b; do git ls-tree -r $r; done) | awk '$2=="blob"{print $3}' | sort -u > /tmp/oids_ripgrep.txt
wc -l < /tmp/oids_ripgrep.txt
for w in 10 50; do /usr/bin/time -f "window=$w %es %MKB" git pack-objects --stdout --window=$w --depth=50 --threads=1 < /tmp/oids_ripgrep.txt | wc -c; done
git pack-objects --stdout --window=0 --threads=1 < /tmp/oids_ripgrep.txt | wc -c
echo == tokio
cd $R/tokio
(for r in bc64194be17bc9711b4a56364e677d823c7cc3d1 74670f287e07c631163545f6be58ce1bec7b7d3a f7681639822262d2566ef07031aed7b7f1d86f5a 36f1a19ac88f36c3d855245f36d272e940446e67 b62d224fac1979a2841d8f0cc083d700eac4429d 804dbd6f8eb6ca65cc67a55048b4579ef1a457aa 8471e0a0ee7f6c973fb517ccb7efcf6c7e2ddc6f db0d6d75b3b65812837c8561269e55eab00a74dd ce173fdc918c5934561560a97d366fce6edb36d0 c9d2a36c7b1dad502d811c4e9082e43c6dbe73c5 bd1e4aaea691eaa9354d467cc606e24050dac9cf 221bb94b9c78f137cb0675f62b6d2e26cdf8d987 cbbf81b9227957dda7efada2265e34dadec554b3 623483c81f545aabf314d2aa005a037db032a911 f3949cc56dfce415c61e0a7a2d7e5e6df188aa4f fe7285d3d167e7b0ff611d7dfc99fe8b00932015 d83ba30d8d426c1ce4363190fbb718fd086928d1 5471a5835e8bc406c2adc2b66d3020c00ab6a685 bfd4ddf597b3edec456fa2d97d4a702452945b6b b499257ef62bd631b83a988d371f4330f545670c; do git ls-tree -r $r; done) | awk '$2=="blob"{print $3}' | sort -u > /tmp/oids_tokio.txt
wc -l < /tmp/oids_tokio.txt
for w in 10 50; do /usr/bin/time -f "window=$w %es %MKB" git pack-objects --stdout --window=$w --depth=50 --threads=1 < /tmp/oids_tokio.txt | wc -c; done
git pack-objects --stdout --window=0 --threads=1 < /tmp/oids_tokio.txt | wc -c
echo == django
cd $R/django
(for r in d6ded0e91bcdd2a8f7a221f6a5552a33fe545359 29f55c8dbb298bcaa5af0b3949cd1a1c39620b8d eba5703fee221bc2ace9fcd2b6873d7e1cdf6753 5b27c3604191eb5c69197abbb98904c643ff457c 2416e5fefe0e853cbfee17ae4dcb62b7055ed2d8 dfcb814a877d9c5fdf67cc0ddc8d96b4f7c1d2db 93fbb77d9b5f4ab936c8f29b20a704d9b11be0c7 3631db88cbae4b50b1d1b0fda0dfaa6a47f4eb0d 8aca2504df9d7d3c1244d1632f6cad45afa60115 b9e0ea3cb4ad25f767c77cb85a05f800aefda65b d6a87eefd87250e68457488cfd62fb2cc8211b24 492537ac18df56c36f8a2335d773aa0fa01a61a3 b0068af5ffc279c281eca3ee078585076e1c7328 75aeebebfe3df3ea46b8e12dd5e7719f98664d3a a4495f4b989dc919d80fcf9f38e31e247faa94fb 932bd794b29c24536b6277b61175bd11ca9be234 e07609a0d1cac573890380ee32b85d7743632650 232b60a21b951bd16b8c95b34fcbcbf3ecd89fca a76035e925ff4e6d8676c65cb135c74b993b1039 a05e16a7230fe33d3f43f4a3aaed103cebb42efc; do git ls-tree -r $r; done) | awk '$2=="blob"{print $3}' | sort -u > /tmp/oids_django.txt
wc -l < /tmp/oids_django.txt
for w in 10 50; do /usr/bin/time -f "window=$w %es %MKB" git pack-objects --stdout --window=$w --depth=50 --threads=1 < /tmp/oids_django.txt | wc -c; done
git pack-objects --stdout --window=0 --threads=1 < /tmp/oids_django.txt | wc -c
echo == godot_demo
cd $R/godot-demo-projects
(for r in d38d18006b840af45a256ba4e3655f07bfd8e150 08be1b7f03f19c887510801e4664ad2360f133c0 353dc4d2d69550a51821a80e3c19bcba30ec2504 d9202081aa42f53ee3295ec2603a4e6018e53285 3a38ab4635f0ceb7b62fc6bbb92ab136994d3314 a69b2f7e215b1d5432959091ec90eb2b0044610c 582229e17cb72038a963c122ac2721bb572f7f49 1cd5f27f8e2189f04b6c0c43e7ec9a23990df9db 520b4a787023e5c4172d448b46a16ecc707224f5 b761b4cd5718a7118f2c95eb55f181d0a5f004f1; do git ls-tree -r $r; done) | awk '$2=="blob"{print $3}' | sort -u > /tmp/oids_godot_demo.txt
wc -l < /tmp/oids_godot_demo.txt
for w in 10 50; do /usr/bin/time -f "window=$w %es %MKB" git pack-objects --stdout --window=$w --depth=50 --threads=1 < /tmp/oids_godot_demo.txt | wc -c; done
git pack-objects --stdout --window=0 --threads=1 < /tmp/oids_godot_demo.txt | wc -c
echo == linux_fs
cd $R/linux
(for r in 1da177e4c3f41524e886b7f1b8a0c1fc7321cac2 4d7b84d1cbb213fdfd2b25ffd1217f249ba1d55a e96e69942312314c061eb2fdd947a7a1211d62f8 2dc9c913154b64efa8346e81cf298012f090c1b1 6707fbb56c8fd3121e334291d170934bcaca2e7f ce00f7feb0a497b4280e1efe16e03728ed292687 7c17e486e865d616f0e37c7f7f0e4dcfab704cd8 12f1d7e4932a40100177be5681f1a26f46265217 1c0490ce902206f4685f812fa81304fd1adf4e35 a51aec4109300d843bf144579109d5288856f72a bbeb1219eeeeab7ef302fdaedee71b08e413a04c cbbddb6b08cc9900a87cccaa9b22bbe06aa4f50a; do git ls-tree -r $r -- fs; done) | awk '$2=="blob"{print $3}' | sort -u > /tmp/oids_linux_fs.txt
wc -l < /tmp/oids_linux_fs.txt
for w in 10 50; do /usr/bin/time -f "window=$w %es %MKB" git pack-objects --stdout --window=$w --depth=50 --threads=1 < /tmp/oids_linux_fs.txt | wc -c; done
git pack-objects --stdout --window=0 --threads=1 < /tmp/oids_linux_fs.txt | wc -c
