pw-dump | python3 -c "                                                                                       
  import json,sys                                                                                              
  for o in json.load(sys.stdin):                                                                               
    if o.get('type')=='PipeWire:Interface:Node':                                                               
      p=o.get('info',{}).get('props',{})                                                                       
      mc=p.get('media.class','')                                                                               
      if 'Stream' in mc or 'Screen' in mc or 'Video' in mc:                                                    
        print(f\"id={o['id']} class={mc} app={p.get('application.name','')}                                    
  pid={p.get('application.process.id','')} target={p.get('target.object','')}\")                               
  "
