def clamp(lo;hi): if . < lo then lo elif . > hi then hi else . end;
def pieces:
  select(all(.[]; . != null)) | select(.[1] <= 90 and .[3] >= -90)
  | [ (.[0]|clamp(-180;180)), (.[1]|clamp(-90;90)), (.[2]|clamp(-180;180)), (.[3]|clamp(-90;90)) ]
  | if .[0] > .[2] then ([.[0],.[1],180,.[3]], [-180,.[1],.[2],.[3]]) else . end;
def hits($w;$s;$e;$n): [(.extent.spatial.bbox//[])[] | pieces | select(.[0] <= $e and .[2] >= $w and .[1] <= $n and .[3] >= $s)] | length > 0;
select(._state != "seed" and ._state != "unregistered" and .visibility=="public" and .disabled != true)
