import os
d=os.path.dirname(os.path.abspath(__file__))
r=lambda n:open(os.path.join(d,n)).read()
out='<title>Jani Missions</title>\n<style>\n'+r('styles.css')+'\n</style>\n'+r('body.html')+'\n<script>\n'+r('a_data.js')+'\n'+r('b_engine.js')+'\n'+r('c_views.js')+'\n</script>\n'
open(os.path.join(d,'artifact.html'),'w').write(out)
print(len(out))
